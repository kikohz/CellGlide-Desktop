using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;

namespace CellGlide.Probe;

/// <summary>One probe command, as given on the command line.</summary>
public sealed record ProbeOptions(string Command, int Seconds, string Lang)
{
    public const string Usage = """
        usage: CellGlide.Probe <command> [--lang TAG]
          status [--seconds N]  print every state and service_gone for N seconds (default 30)
          speed-small           a small speed test (10 MB down, 5 MB up, 5 s), phone and Wi-Fi legs
          leak                  the DNS leak test
        exit code: 0 ok, 1 a leg or the leak test failed, 2 usage, timeout or the DLL refused,
                   130 interrupted (Ctrl+C; a running test is cancelled first)
        """;

    /// <summary>The options in <paramref name="args"/>, or null when they are not a valid command line.</summary>
    public static ProbeOptions? Parse(IReadOnlyList<string> args)
    {
        if (args.Count == 0 || args[0] is not ("status" or "speed-small" or "leak"))
        {
            return null;
        }
        var command = args[0];
        var seconds = 30;
        var lang = "";
        for (var i = 1; i < args.Count; i++)
        {
            if (args[i] == "--seconds" && command == "status" && i + 1 < args.Count
                && int.TryParse(args[i + 1], out var n) && n > 0)
            {
                seconds = n;
                i++;
            }
            else if (args[i] == "--lang" && i + 1 < args.Count)
            {
                lang = args[i + 1];
                i++;
            }
            else
            {
                return null;
            }
        }
        return new ProbeOptions(command, seconds, lang);
    }
}

/// <summary>
/// Runs one probe command over an <see cref="EventHub"/>. Everything printed after the line tag
/// (state, service_gone, speed, ...) is text the DLL gave; the tags are the event names of the
/// DLL's protocol. Every wait ends on its own bound or when <c>stop</c> is signalled; a test that
/// is running then is cancelled before returning <see cref="Interrupted"/>.
/// </summary>
public sealed class ProbeRunner
{
    public const int Ok = 0;
    public const int Failed = 1;
    public const int Trouble = 2;
    public const int Interrupted = 130;

    public static readonly SpeedCaps SmallCaps = new(DownloadBytes: 10_000_000, UploadBytes: 5_000_000, Seconds: 5);

    readonly EventHub _hub;
    readonly TextWriter _output;
    readonly Lock _outputGate = new();
    string? _lastState;

    public ProbeRunner(EventHub hub, TextWriter output)
    {
        _hub = hub;
        _output = output;
    }

    // Bounds for one run; a small test and the leak test finish well inside them.
    public TimeSpan FirstStateWithin { get; init; } = TimeSpan.FromSeconds(30);
    public TimeSpan ReplyWithin { get; init; } = TimeSpan.FromSeconds(15);
    public TimeSpan SpeedWithin { get; init; } = TimeSpan.FromSeconds(180);
    public TimeSpan LeakWithin { get; init; } = TimeSpan.FromSeconds(120);
    public TimeSpan CancelWithin { get; init; } = TimeSpan.FromSeconds(5);

    public Task<int> RunAsync(ProbeOptions options, CancellationToken stop) => options.Command switch
    {
        "status" => Status(options.Seconds, stop),
        "speed-small" => SpeedSmall(stop),
        "leak" => Leak(stop),
        _ => Task.FromResult(Trouble),
    };

    void Print(string tag, string text)
    {
        lock (_outputGate)
        {
            _output.WriteLine($"{DateTime.Now:HH:mm:ss.fff} {tag,-13} {text}".TrimEnd());
        }
    }

    static string StateLine(OverviewView v)
    {
        var parts = new List<string> { v.Title, v.Detail };
        if (v.StrictDns is { } strict)
        {
            parts.Add(strict);
        }
        if (v.Phone is { } phone)
        {
            parts.Add(phone.Radio is { } radio ? $"{phone.Title} · {radio}" : phone.Title);
        }
        return string.Join(" | ", parts);
    }

    enum Ended
    {
        Done,
        TimedOut,
        Stopped,
    }

    /// <summary>Waits for <paramref name="task"/>, at most <paramref name="within"/>, until <paramref name="stop"/>.</summary>
    static async Task<Ended> Wait(Task task, TimeSpan within, CancellationToken stop)
    {
        using var timer = CancellationTokenSource.CreateLinkedTokenSource(stop);
        var first = await Task.WhenAny(task, Task.Delay(within, timer.Token)).ConfigureAwait(false);
        await timer.CancelAsync().ConfigureAwait(false);
        if (first == task)
        {
            return Ended.Done;
        }
        return stop.IsCancellationRequested ? Ended.Stopped : Ended.TimedOut;
    }

    /// <summary>Sends <paramref name="command"/> and waits for its reply; null (with the reason printed) when it never came.</summary>
    async Task<(ReplyResult? Reply, Ended Ended)> Send(Command command, CancellationToken stop)
    {
        var reply = _hub.SendAsync(command);
        var ended = await Wait(reply, ReplyWithin, stop).ConfigureAwait(false);
        if (ended == Ended.TimedOut)
        {
            Print("timeout", $"no reply within {ReplyWithin.TotalSeconds:0} s");
        }
        return ended == Ended.Done ? (await reply.ConfigureAwait(false), ended) : (null, ended);
    }

    /// <summary>Prints the DLL's reply when it is an error; true when it is.</summary>
    bool Refused(ReplyResult reply)
    {
        if (reply is ReplyErr err)
        {
            Print("reply_err", err.Message);
            return true;
        }
        return false;
    }

    /// <summary>Completes on the next state; its handler then leaves the hub.</summary>
    Task NextState()
    {
        var next = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        Action<OverviewView>? once = null;
        once = _ =>
        {
            _hub.State -= once;
            next.TrySetResult();
        };
        _hub.State += once;
        return next.Task;
    }

    /// <summary>
    /// Prints states (consecutive identical ones once), service_gone and phone_plugged, and
    /// subscribes. <paramref name="waitForState"/>: also waits for the first state, so the DLL
    /// knows how the connection stands. Returns the exit code to stop with, or null to go on.
    /// </summary>
    async Task<int?> Subscribe(bool waitForState, CancellationToken stop)
    {
        _hub.State += v =>
        {
            var line = StateLine(v);
            if (line != _lastState)
            {
                _lastState = line;
                Print("state", line);
            }
        };
        _hub.ServiceGone += e =>
        {
            _lastState = null;
            Print("service_gone", e.Text);
        };
        _hub.PhonePlugged += serial => Print("phone_plugged", serial);
        _hub.Fault += message => Print("fault", message);
        var first = NextState();
        var (reply, ended) = await Send(new SubscribeCommand(), stop).ConfigureAwait(false);
        if (reply is null)
        {
            return ended == Ended.Stopped ? Interrupted : Trouble;
        }
        if (Refused(reply))
        {
            return Trouble;
        }
        if (!waitForState)
        {
            return null;
        }
        switch (await Wait(first, FirstStateWithin, stop).ConfigureAwait(false))
        {
            case Ended.Stopped:
                return Interrupted;
            case Ended.TimedOut:
                Print("timeout", $"no state within {FirstStateWithin.TotalSeconds:0} s");
                return Trouble;
            default:
                return null;
        }
    }

    /// <summary>Cancels a test that may be running and waits briefly for the DLL to take it.</summary>
    async Task Cancel(Command cancel)
    {
        await Wait(_hub.SendAsync(cancel), CancelWithin, CancellationToken.None).ConfigureAwait(false);
    }

    async Task<int> Status(int seconds, CancellationToken stop)
    {
        if (await Subscribe(waitForState: false, stop).ConfigureAwait(false) is { } code)
        {
            return code;
        }
        // Nothing to finish: the run lasts its seconds unless stopped first.
        var never = new TaskCompletionSource().Task;
        if (await Wait(never, TimeSpan.FromSeconds(seconds), stop).ConfigureAwait(false) == Ended.Stopped)
        {
            Print("interrupted", "");
            return Interrupted;
        }
        return Ok;
    }

    /// <summary>A page that says how the run ended, even if it never said it was running.</summary>
    static bool IsOver(SpeedView v) =>
        !v.Running && v.Legs.Count > 0
        && v.Legs.All(l => l.Stage is StageView.Done or StageView.Failed or StageView.Cancelled);

    static bool IsOver(LeakView v) => !v.Running && (v.Result is not null || v.Error is not null);

    /// <summary>
    /// Sends <paramref name="start"/> and waits for <paramref name="finished"/>. Returns the exit
    /// code to stop with, or null once finished.
    /// </summary>
    async Task<int?> RunTest(Command start, Command cancel, Task finished, TimeSpan within, CancellationToken stop)
    {
        var (reply, ended) = await Send(start, stop).ConfigureAwait(false);
        if (reply is not null && Refused(reply))
        {
            return Trouble;
        }
        if (reply is not null)
        {
            ended = await Wait(finished, within, stop).ConfigureAwait(false);
            if (ended == Ended.TimedOut)
            {
                Print("timeout", $"no result within {within.TotalSeconds:0} s");
            }
        }
        if (ended == Ended.Done)
        {
            return null;
        }
        // Stopped or timed out once the start was sent: the test may be running, so cancel it.
        await Cancel(cancel).ConfigureAwait(false);
        if (ended == Ended.Stopped)
        {
            Print("interrupted", "");
            return Interrupted;
        }
        return Trouble;
    }

    async Task<int> SpeedSmall(CancellationToken stop)
    {
        if (await Subscribe(waitForState: true, stop).ConfigureAwait(false) is { } code)
        {
            return code;
        }
        var sawRunning = false;
        var lastLines = new Dictionary<int, string>();
        var finished = new TaskCompletionSource<SpeedView>(TaskCreationOptions.RunContinuationsAsynchronously);
        _hub.SpeedProgress += v =>
        {
            if (v.Dialog is not null)
            {
                // Confirmed is sent, so the DLL should never ask; say so rather than wait.
                Print("speed", $"dialog {v.Dialog}");
                finished.TrySetResult(v);
                return;
            }
            for (var i = 0; i < v.Legs.Count; i++)
            {
                var leg = v.Legs[i];
                var line = $"{leg.Label} | {leg.StageText} | {leg.Numbers}";
                if (!lastLines.TryGetValue(i, out var previous) || previous != line)
                {
                    lastLines[i] = line;
                    Print("speed", line);
                }
            }
            sawRunning |= v.Running;
            if (!v.Running && (sawRunning || IsOver(v)))
            {
                finished.TrySetResult(v);
            }
        };
        var start = new SpeedStartCommand(IncludeWifi: true, Confirmed: true, Caps: SmallCaps);
        if (await RunTest(start, new SpeedCancelCommand(), finished.Task, SpeedWithin, stop).ConfigureAwait(false) is { } stopped)
        {
            return stopped;
        }
        var view = await finished.Task.ConfigureAwait(false);
        if (view.Dialog is not null)
        {
            await Cancel(new SpeedCancelCommand()).ConfigureAwait(false);
            return Trouble;
        }
        foreach (var leg in view.Legs)
        {
            Print("leg", $"{leg.Label} | {leg.Stage} | {leg.Numbers}");
            if (leg.Error is { } error)
            {
                Print("leg_error", error);
            }
            foreach (var note in leg.Notes)
            {
                Print("leg_note", note);
            }
        }
        if (view.Verdict is { } verdict)
        {
            Print("verdict", verdict);
        }
        if (view.History.Count > 0)
        {
            var r = view.History[0];
            Print("history", string.Join(" | ", new[] { r.Date, r.Phone, r.Wifi, r.Carrier }.OfType<string>()));
        }
        return view.Legs.Count > 0 && view.Legs.All(l => l.Stage == StageView.Done) ? Ok : Failed;
    }

    async Task<int> Leak(CancellationToken stop)
    {
        if (await Subscribe(waitForState: true, stop).ConfigureAwait(false) is { } code)
        {
            return code;
        }
        var sawRunning = false;
        var finished = new TaskCompletionSource<LeakView>(TaskCreationOptions.RunContinuationsAsynchronously);
        _hub.LeakProgress += v =>
        {
            sawRunning |= v.Running;
            if (!v.Running && (sawRunning || IsOver(v)))
            {
                finished.TrySetResult(v);
            }
        };
        if (await RunTest(new LeakStartCommand(), new LeakCancelCommand(), finished.Task, LeakWithin, stop).ConfigureAwait(false) is { } stopped)
        {
            return stopped;
        }
        var view = await finished.Task.ConfigureAwait(false);
        if (view.Result is { } result)
        {
            Print("leak_title", result.Title);
            Print("leak_detail", result.Detail);
            Print("leak_you", result.You);
            foreach (var group in result.Groups)
            {
                Print("leak_group", $"{group.Name} {string.Join(' ', group.Addresses)}");
            }
        }
        if (view.Error is { } error)
        {
            Print("leak_error", error);
            return Failed;
        }
        return view.Result is null ? Failed : Ok;
    }
}
