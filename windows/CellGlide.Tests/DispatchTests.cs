using System.Collections.Concurrent;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;

namespace CellGlide.Tests;

/// <summary>A minimal single-threaded SynchronizationContext with a thread of its own, as a UI thread.</summary>
sealed class QueueContext : SynchronizationContext, IDisposable
{
    readonly BlockingCollection<(SendOrPostCallback, object?)> _queue = [];
    readonly Thread _thread;

    public QueueContext()
    {
        _thread = new Thread(() =>
        {
            SetSynchronizationContext(this);
            foreach (var (callback, state) in _queue.GetConsumingEnumerable())
            {
                callback(state);
            }
        })
        { IsBackground = true, Name = "test-ui" };
        _thread.Start();
    }

    public int ThreadId => _thread.ManagedThreadId;

    public override void Post(SendOrPostCallback d, object? state) => _queue.Add((d, state));

    public void Dispose()
    {
        _queue.CompleteAdding();
        _thread.Join();
    }
}

/// <summary>Replies to each command from another thread, after a pause, while Send has not yet returned.</summary>
sealed class LateSendCore : ICore
{
    ulong _next;

    public event Action<CoreEvent>? EventReceived;

    public ulong Send(Command command)
    {
        var id = Interlocked.Increment(ref _next);
        new Thread(() => EventReceived?.Invoke(new ReplyEvent(id, new ReplyErr($"reply {id}")))).Start();
        Thread.Sleep(50);
        return id;
    }

    public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => key;

    public void Dispose()
    {
    }
}

public class DispatchTests
{
    [Fact]
    public async Task Events_reach_the_ui_thread()
    {
        using var ui = new QueueContext();
        var core = new FakeCore();
        var hub = new EventHub(core, new SynchronizationContextDispatcher(ui));
        var ran = new TaskCompletionSource<int>();
        hub.State += _ => ran.SetResult(Environment.CurrentManagedThreadId);
        await Task.Run(() => core.Raise(new StateEvent(Samples.Overview())), TestContext.Current.CancellationToken);
        Assert.Equal(ui.ThreadId, await ran.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
    }

    [Fact]
    public async Task Replies_go_through_the_dispatcher()
    {
        var ui = new HeldDispatcher();
        var core = new FakeCore();
        var hub = new EventHub(core, ui);
        var send = hub.SendAsync(new GetPrefsCommand());
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyOk(new DoneValue())));
        // Nothing is completed on the DLL's thread: only once the UI thread runs what was posted.
        Assert.False(send.IsCompleted);
        ui.RunAll();
        Assert.IsType<ReplyOk>(await send.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
    }

    /// <summary>Keeps what is posted until the test runs it.</summary>
    sealed class HeldDispatcher : IUiDispatcher
    {
        readonly List<Action> _held = [];

        public void Post(Action action) => _held.Add(action);

        public void RunAll()
        {
            foreach (var action in _held)
            {
                action();
            }
            _held.Clear();
        }
    }

    [Fact]
    public async Task SendAsync_completes_with_its_own_reply()
    {
        var core = new FakeCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var a = hub.SendAsync(new GetPrefsCommand());
        var b = hub.SendAsync(new SpeedCancelCommand());
        core.Raise(new ReplyEvent(core.SentIds[1], new ReplyErr("b")));
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyOk(new DoneValue())));
        Assert.IsType<ReplyOk>(await a);
        Assert.Equal("b", Assert.IsType<ReplyErr>(await b).Message);
    }

    [Fact]
    public async Task A_reply_before_send_returns_is_kept()
    {
        var inline = new EventHub(new FakeCore(new ReplyErr("inline")), new InlineDispatcher());
        Assert.Equal("inline", Assert.IsType<ReplyErr>(await inline.SendAsync(new ToggleCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken)).Message);

        var late = new EventHub(new LateSendCore(), new InlineDispatcher());
        var reply = await late.SendAsync(new ToggleCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Equal("reply 1", Assert.IsType<ReplyErr>(reply).Message);
    }

    [Fact]
    public async Task Events_keep_their_order()
    {
        using var ui = new QueueContext();
        var core = new FakeCore();
        var hub = new EventHub(core, new SynchronizationContextDispatcher(ui));
        var serials = new List<string>();
        var done = new TaskCompletionSource();
        hub.PhonePlugged += serial =>
        {
            serials.Add(serial);
            if (serials.Count == 100)
            {
                done.SetResult();
            }
        };
        await Task.Run(() =>
        {
            for (var i = 0; i < 100; i++)
            {
                core.Raise(new PhonePluggedEvent(i.ToString(System.Globalization.CultureInfo.InvariantCulture)));
            }
        }, TestContext.Current.CancellationToken);
        await done.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Equal(Enumerable.Range(0, 100).Select(i => i.ToString(System.Globalization.CultureInfo.InvariantCulture)), serials);
    }

    [Fact]
    public void Each_event_has_its_own_hub_event()
    {
        var core = new FakeCore();
        var hub = new EventHub(core, new InlineDispatcher());
        var got = new List<string>();
        hub.State += _ => got.Add("state");
        hub.ServiceGone += _ => got.Add("service_gone");
        hub.PhonePlugged += _ => got.Add("phone_plugged");
        hub.SpeedProgress += _ => got.Add("speed_progress");
        hub.LeakProgress += _ => got.Add("leak_progress");
        hub.PrefsChanged += _ => got.Add("prefs");
        hub.Fault += _ => got.Add("fault");
        foreach (var name in new[] { "state_connected", "service_gone_stopped", "phone_plugged", "speed_progress_done", "leak_progress_result", "prefs", "fault" })
        {
            core.Raise(Samples.Event(name + ".json"));
        }
        Assert.Equal(["state", "service_gone", "phone_plugged", "speed_progress", "leak_progress", "prefs", "fault"], got);
    }

    [Fact]
    public async Task A_core_that_is_not_running_answers_with_an_error()
    {
        var hub = new EventHub(new StoppedCore(), new InlineDispatcher());
        var reply = await hub.SendAsync(new GetPrefsCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Equal("reason.internal", Assert.IsType<ReplyErr>(reply).Message);
    }

    [Fact]
    public async Task A_core_that_is_not_running_answers_through_the_dispatcher()
    {
        var ui = new HeldDispatcher();
        var hub = new EventHub(new StoppedCore(), ui);
        var send = hub.SendAsync(new GetPrefsCommand());
        Assert.False(send.IsCompleted);
        ui.RunAll();
        var reply = await send.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Equal("reason.internal", Assert.IsType<ReplyErr>(reply).Message);
    }

    [Fact]
    public async Task Dispose_cancels_commands_still_waiting()
    {
        var ui = new HeldDispatcher();
        var core = new FakeCore();
        var hub = new EventHub(core, ui);
        var unanswered = hub.SendAsync(new GetPrefsCommand());
        var posted = hub.SendAsync(new ToggleCommand());
        // A reply that reached the dispatcher but never ran on the UI thread is cancelled too.
        core.Raise(new ReplyEvent(core.SentIds[1], new ReplyOk(new DoneValue())));
        hub.Dispose();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => unanswered.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
        ui.RunAll();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => posted.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
    }

    [Fact]
    public async Task Sending_after_dispose_is_cancelled_and_sends_nothing()
    {
        var core = new FakeCore();
        var hub = new EventHub(core, new InlineDispatcher());
        hub.Dispose();
        hub.Dispose();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => hub.SendAsync(new GetPrefsCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
        Assert.Empty(core.Sent);
        var states = 0;
        hub.State += _ => states++;
        core.Raise(new StateEvent(Samples.Overview()));
        Assert.Equal(0, states);
    }

    [Fact]
    public async Task A_reply_the_dispatcher_refuses_faults_its_command()
    {
        var core = new FakeCore();
        var hub = new EventHub(core, new RefusingDispatcher());
        var send = hub.SendAsync(new GetPrefsCommand());
        core.Raise(new ReplyEvent(core.SentIds[0], new ReplyOk(new DoneValue())));
        var thrown = await Assert.ThrowsAsync<InvalidOperationException>(() => send.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
        Assert.Equal("ui thread gone", thrown.Message);

        var inline = new EventHub(new FakeCore(new ReplyOk(new DoneValue())), new RefusingDispatcher());
        await Assert.ThrowsAsync<InvalidOperationException>(() => inline.SendAsync(new GetPrefsCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
        var stopped = new EventHub(new StoppedCore(), new RefusingDispatcher());
        await Assert.ThrowsAsync<InvalidOperationException>(() => stopped.SendAsync(new GetPrefsCommand()).WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
    }

    /// <summary>A dispatcher whose UI thread has gone away.</summary>
    sealed class RefusingDispatcher : IUiDispatcher
    {
        public void Post(Action action) => throw new InvalidOperationException("ui thread gone");
    }

    sealed class StoppedCore : ICore
    {
        public event Action<CoreEvent>? EventReceived
        {
            add { }
            remove { }
        }

        public ulong Send(Command command) => 0;

        public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => key;

        public void Dispose()
        {
        }
    }
}
