using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Probe;

namespace CellGlide.Tests;

/// <summary>An ICore that answers each command inline with the events a test scripts, then Done.</summary>
sealed class ScriptedCore(Func<Command, IEnumerable<CoreEvent>> script) : ICore
{
    readonly Lock _gate = new();
    ulong _next;

    public List<Command> Sent { get; } = [];

    readonly List<(Type Kind, TaskCompletionSource Seen)> _waiting = [];

    /// <summary>Completes once a command of type <typeparamref name="T"/> has been sent.</summary>
    public Task WhenSent<T>() where T : Command
    {
        lock (_gate)
        {
            if (Sent.Any(c => c is T))
            {
                return Task.CompletedTask;
            }
            var seen = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _waiting.Add((typeof(T), seen));
            return seen.Task;
        }
    }

    public event Action<CoreEvent>? EventReceived;

    public ulong Send(Command command)
    {
        ulong id;
        lock (_gate)
        {
            id = ++_next;
            Sent.Add(command);
            foreach (var w in _waiting.Where(w => w.Kind == command.GetType()))
            {
                w.Seen.TrySetResult();
            }
        }
        foreach (var ev in script(command))
        {
            EventReceived?.Invoke(ev);
        }
        EventReceived?.Invoke(new ReplyEvent(id, new ReplyOk(new DoneValue())));
        return id;
    }

    public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => key;

    public void Dispose()
    {
    }
}

public class ProbeTests
{
    static readonly TimeSpan Prompt = TimeSpan.FromSeconds(5);

    static (ProbeRunner Runner, ScriptedCore Core, StringWriter Output) Probe(Func<Command, IEnumerable<CoreEvent>> script)
    {
        var core = new ScriptedCore(script);
        var output = new StringWriter();
        var hub = new EventHub(core, new InlineDispatcher());
        // Long bounds: a test that passes did not get there by timing out.
        var runner = new ProbeRunner(hub, output)
        {
            FirstStateWithin = TimeSpan.FromMinutes(5),
            ReplyWithin = TimeSpan.FromMinutes(5),
            SpeedWithin = TimeSpan.FromMinutes(5),
            LeakWithin = TimeSpan.FromMinutes(5),
        };
        return (runner, core, output);
    }

    /// <summary>Subscribe gets a state; the start command gets <paramref name="onStart"/>.</summary>
    static Func<Command, IEnumerable<CoreEvent>> Script(Type start, params string[] onStart) => command =>
        command switch
        {
            SubscribeCommand => [Samples.Event("state_connected.json")],
            _ when command.GetType() == start => onStart.Select(Samples.Event),
            _ => [],
        };

    static ProbeOptions Options(params string[] args) => ProbeOptions.Parse(args)!;

    static async Task<int> Within(Task<int> run)
    {
        Assert.Same(run, await Task.WhenAny(run, Task.Delay(Prompt)));
        return await run;
    }

    [Fact]
    public async Task Ctrl_c_during_a_speed_test_cancels_it_and_exits_130()
    {
        var (runner, core, output) = Probe(Script(typeof(SpeedStartCommand), "speed_progress_confirming.json"));
        using var stop = new CancellationTokenSource();
        var run = runner.RunAsync(Options("speed-small"), stop.Token);
        await core.WhenSent<SpeedStartCommand>().WaitAsync(Prompt, TestContext.Current.CancellationToken);
        Assert.False(run.IsCompleted);
        stop.Cancel();
        Assert.Equal(ProbeRunner.Interrupted, await Within(run));
        Assert.IsType<SpeedCancelCommand>(core.Sent[^1]);
        var started = Assert.IsType<SpeedStartCommand>(core.Sent[^2]);
        Assert.Equal(ProbeRunner.SmallCaps, started.Caps);
        Assert.Contains("interrupted", output.ToString());
    }

    [Fact]
    public async Task Ctrl_c_during_the_leak_test_cancels_it_and_exits_130()
    {
        var (runner, core, _) = Probe(Script(typeof(LeakStartCommand), "leak_progress_running.json"));
        using var stop = new CancellationTokenSource();
        var run = runner.RunAsync(Options("leak"), stop.Token);
        await core.WhenSent<LeakStartCommand>().WaitAsync(Prompt, TestContext.Current.CancellationToken);
        Assert.False(run.IsCompleted);
        stop.Cancel();
        Assert.Equal(ProbeRunner.Interrupted, await Within(run));
        Assert.IsType<LeakCancelCommand>(core.Sent[^1]);
    }

    [Fact]
    public async Task Ctrl_c_before_the_first_state_starts_nothing()
    {
        var (runner, core, _) = Probe(_ => []);
        using var stop = new CancellationTokenSource();
        var run = runner.RunAsync(Options("speed-small"), stop.Token);
        stop.Cancel();
        Assert.Equal(ProbeRunner.Interrupted, await Within(run));
        Assert.All(core.Sent, c => Assert.IsType<SubscribeCommand>(c));
    }

    [Fact]
    public async Task Ctrl_c_ends_status_early()
    {
        var (runner, _, _) = Probe(Script(typeof(SubscribeCommand)));
        using var stop = new CancellationTokenSource();
        var run = runner.RunAsync(Options("status", "--seconds", "600"), stop.Token);
        stop.Cancel();
        Assert.Equal(ProbeRunner.Interrupted, await Within(run));
    }

    [Fact]
    public async Task Status_runs_its_seconds_and_prints_the_state()
    {
        var (runner, _, output) = Probe(Script(typeof(SubscribeCommand)));
        var code = await Within(runner.RunAsync(Options("status", "--seconds", "1"), CancellationToken.None));
        Assert.Equal(ProbeRunner.Ok, code);
        Assert.Contains(Samples.Overview().Title, output.ToString());
    }

    [Fact]
    public async Task A_speed_test_that_fails_before_it_runs_exits_1_at_once()
    {
        var (runner, _, _) = Probe(Script(typeof(SpeedStartCommand), "speed_progress_failed.json"));
        Assert.Equal(ProbeRunner.Failed, await Within(runner.RunAsync(Options("speed-small"), CancellationToken.None)));
    }

    [Fact]
    public async Task A_speed_test_that_runs_and_ends_done_exits_0()
    {
        var (runner, _, _) = Probe(Script(typeof(SpeedStartCommand), "speed_progress_latency.json", "speed_progress_done.json"));
        Assert.Equal(ProbeRunner.Ok, await Within(runner.RunAsync(Options("speed-small"), CancellationToken.None)));
    }

    [Fact]
    public async Task A_leak_test_that_fails_before_it_runs_exits_1_at_once()
    {
        var (runner, _, output) = Probe(Script(typeof(LeakStartCommand), "leak_progress_failed.json"));
        Assert.Equal(ProbeRunner.Failed, await Within(runner.RunAsync(Options("leak"), CancellationToken.None)));
        Assert.Contains("leak_error", output.ToString());
    }

    [Fact]
    public async Task A_leak_result_exits_0()
    {
        var (runner, _, _) = Probe(Script(typeof(LeakStartCommand), "leak_progress_running.json", "leak_progress_result.json"));
        Assert.Equal(ProbeRunner.Ok, await Within(runner.RunAsync(Options("leak"), CancellationToken.None)));
    }

    [Theory]
    [InlineData("speed-small", "--seconds", "5")]
    [InlineData("leak", "--seconds", "5")]
    [InlineData("status", "--seconds", "0")]
    [InlineData("status", "--seconds")]
    [InlineData("speed")]
    [InlineData]
    public void Bad_command_lines_are_usage_errors(params string[] args) => Assert.Null(ProbeOptions.Parse(args));

    [Fact]
    public void Good_command_lines_parse()
    {
        Assert.Equal(new ProbeOptions("status", 60, "zh-CN"), ProbeOptions.Parse(["status", "--seconds", "60", "--lang", "zh-CN"]));
        Assert.Equal(new ProbeOptions("leak", 30, ""), ProbeOptions.Parse(["leak"]));
    }
}
