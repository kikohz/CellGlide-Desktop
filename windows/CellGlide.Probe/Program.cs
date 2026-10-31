// CellGlide.Probe: drives cellglide_native.dll through CellGlide.Core from a console.
//
//   dotnet run --project CellGlide.Probe -- status [--seconds N] [--lang TAG]
//   dotnet run --project CellGlide.Probe -- speed-small [--lang TAG]
//   dotnet run --project CellGlide.Probe -- leak [--lang TAG]
//
// Everything printed after the line tag (state, service_gone, speed, ...) is text the DLL gave;
// the tags are the event names of the DLL's protocol. Only the usage text is written here.
using System.Text;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.Native;

namespace CellGlide.Probe;

static class Program
{
    const string Usage = """
        usage: CellGlide.Probe <command> [--lang TAG]
          status [--seconds N]  print every state and service_gone for N seconds (default 30)
          speed-small           a small speed test (10 MB down, 5 MB up, 5 s), phone and Wi-Fi legs
          leak                  the DNS leak test
        exit code: 0 ok, 1 a leg or the leak test failed, 2 usage, timeout or the DLL refused
        """;

    // Bounds for one run; a small test and the leak test finish well inside them.
    static readonly TimeSpan FirstStateWithin = TimeSpan.FromSeconds(30);
    static readonly TimeSpan SpeedWithin = TimeSpan.FromSeconds(180);
    static readonly TimeSpan LeakWithin = TimeSpan.FromSeconds(120);

    static readonly SpeedCaps SmallCaps = new(DownloadBytes: 10_000_000, UploadBytes: 5_000_000, Seconds: 5);

    static async Task<int> Main(string[] args)
    {
        Console.OutputEncoding = Encoding.UTF8;
        if (args.Length == 0)
        {
            Console.Error.WriteLine(Usage);
            return 2;
        }
        var command = args[0];
        var seconds = 30;
        var lang = "";
        for (var i = 1; i < args.Length; i++)
        {
            if (args[i] == "--seconds" && i + 1 < args.Length && int.TryParse(args[i + 1], out var n) && n > 0)
            {
                seconds = n;
                i++;
            }
            else if (args[i] == "--lang" && i + 1 < args.Length)
            {
                lang = args[i + 1];
                i++;
            }
            else
            {
                Console.Error.WriteLine(Usage);
                return 2;
            }
        }
        if (command is not ("status" or "speed-small" or "leak"))
        {
            Console.Error.WriteLine(Usage);
            return 2;
        }

        using var core = NativeCore.Start(lang);
        using var hub = new EventHub(core, new InlineDispatcher());
        hub.Fault += message => Print("fault", message);
        return command switch
        {
            "status" => await Status(hub, seconds),
            "speed-small" => await SpeedSmall(hub),
            _ => await Leak(hub),
        };
    }

    static readonly Lock OutputGate = new();

    static void Print(string tag, string text)
    {
        lock (OutputGate)
        {
            Console.WriteLine($"{DateTime.Now:HH:mm:ss.fff} {tag,-13} {text}");
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

    /// <summary>Prints the DLL's reply when it is an error; true when it is.</summary>
    static bool Refused(ReplyResult reply)
    {
        if (reply is ReplyErr err)
        {
            Print("reply_err", err.Message);
            return true;
        }
        return false;
    }

    /// <summary>Subscribes and waits for the first state, so the DLL knows how the connection stands.</summary>
    static async Task<bool> SubscribeAndWait(EventHub hub)
    {
        var first = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        hub.State += v =>
        {
            if (first.TrySetResult())
            {
                Print("state", StateLine(v));
            }
        };
        hub.ServiceGone += e => Print("service_gone", e.Text);
        if (Refused(await hub.SendAsync(new SubscribeCommand())))
        {
            return false;
        }
        if (await Task.WhenAny(first.Task, Task.Delay(FirstStateWithin)) != first.Task)
        {
            Print("timeout", $"no state within {FirstStateWithin.TotalSeconds:0} s");
            return false;
        }
        return true;
    }

    static async Task<int> Status(EventHub hub, int seconds)
    {
        // Consecutive identical states are printed once.
        string? last = null;
        hub.State += v =>
        {
            var line = StateLine(v);
            if (line != last)
            {
                last = line;
                Print("state", line);
            }
        };
        hub.ServiceGone += e =>
        {
            last = null;
            Print("service_gone", e.Text);
        };
        hub.PhonePlugged += serial => Print("phone_plugged", serial);
        if (Refused(await hub.SendAsync(new SubscribeCommand())))
        {
            return 2;
        }
        await Task.Delay(TimeSpan.FromSeconds(seconds));
        return 0;
    }

    static async Task<int> SpeedSmall(EventHub hub)
    {
        if (!await SubscribeAndWait(hub))
        {
            return 2;
        }
        var started = false;
        var sawRunning = false;
        var lastLines = new Dictionary<int, string>();
        var finished = new TaskCompletionSource<SpeedView>(TaskCreationOptions.RunContinuationsAsynchronously);
        hub.SpeedProgress += v =>
        {
            if (!started)
            {
                return;
            }
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
            if (sawRunning && !v.Running)
            {
                finished.TrySetResult(v);
            }
        };
        started = true;
        if (Refused(await hub.SendAsync(new SpeedStartCommand(IncludeWifi: true, Confirmed: true, Caps: SmallCaps))))
        {
            return 2;
        }
        if (await Task.WhenAny(finished.Task, Task.Delay(SpeedWithin)) != finished.Task)
        {
            Print("timeout", $"no result within {SpeedWithin.TotalSeconds:0} s");
            await hub.SendAsync(new SpeedCancelCommand());
            return 2;
        }
        var view = await finished.Task;
        if (view.Dialog is not null)
        {
            await hub.SendAsync(new SpeedCancelCommand());
            return 2;
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
        return view.Legs.Count > 0 && view.Legs.All(l => l.Stage == StageView.Done) ? 0 : 1;
    }

    static async Task<int> Leak(EventHub hub)
    {
        if (!await SubscribeAndWait(hub))
        {
            return 2;
        }
        var started = false;
        var sawRunning = false;
        var finished = new TaskCompletionSource<LeakView>(TaskCreationOptions.RunContinuationsAsynchronously);
        hub.LeakProgress += v =>
        {
            if (!started)
            {
                return;
            }
            sawRunning |= v.Running;
            if (sawRunning && !v.Running)
            {
                finished.TrySetResult(v);
            }
        };
        started = true;
        if (Refused(await hub.SendAsync(new LeakStartCommand())))
        {
            return 2;
        }
        if (await Task.WhenAny(finished.Task, Task.Delay(LeakWithin)) != finished.Task)
        {
            Print("timeout", $"no result within {LeakWithin.TotalSeconds:0} s");
            await hub.SendAsync(new LeakCancelCommand());
            return 2;
        }
        var view = await finished.Task;
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
            return 1;
        }
        return view.Result is null ? 1 : 0;
    }
}
