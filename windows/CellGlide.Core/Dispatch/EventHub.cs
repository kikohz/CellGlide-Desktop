using CellGlide.Core.Models;

namespace CellGlide.Core.Dispatch;

/// <summary>
/// Takes every event off the DLL's thread and raises it on the UI thread, through
/// <see cref="IUiDispatcher.Post"/>; matches replies to the commands sent with <see cref="SendAsync"/>.
/// Nothing here waits on the DLL's thread, so the DLL may be started or stopped from any handler.
/// </summary>
/// <remarks>
/// Every <see cref="SendAsync"/> task ends: with its reply (completed on the UI thread), faulted with
/// the dispatcher's exception if the reply could not be posted, or cancelled when the hub is disposed.
/// </remarks>
public sealed class EventHub : IDisposable
{
    readonly ICore _core;
    readonly IUiDispatcher _dispatcher;
    readonly Lock _gate = new();
    // Commands still waiting; a reply stays here until the UI thread has completed it, so that
    // Dispose can still cancel a reply that was posted but never run.
    readonly Dictionary<ulong, TaskCompletionSource<ReplyResult>> _pending = [];
    // Replies that came back on the sending thread before Send returned (a core that answers inline).
    readonly Dictionary<ulong, ReplyResult> _early = [];
    int _sendingThread;
    bool _disposed;

    public EventHub(ICore core, IUiDispatcher dispatcher)
    {
        _core = core;
        _dispatcher = dispatcher;
        _core.EventReceived += OnEvent;
    }

    public event Action<OverviewView>? State;
    public event Action<ServiceGoneEvent>? ServiceGone;
    public event Action<string>? PhonePlugged;
    public event Action<SpeedView>? SpeedProgress;
    public event Action<LeakView>? LeakProgress;
    public event Action<Prefs>? PrefsChanged;
    public event Action<string>? Fault;

    /// <summary>The core's text for <paramref name="key"/>, in the current language.</summary>
    public string Tr(string key, IReadOnlyDictionary<string, string>? args = null) => _core.Tr(key, args);

    /// <summary>
    /// Sends <paramref name="command"/>; completes with its own reply, on the UI thread. Cancelled
    /// if the hub is (or gets) disposed first; faulted if the dispatcher refuses the completion.
    /// </summary>
    public Task<ReplyResult> SendAsync(Command command)
    {
        var reply = new TaskCompletionSource<ReplyResult>(TaskCreationOptions.RunContinuationsAsynchronously);
        ulong id;
        ReplyResult? now = null;
        lock (_gate)
        {
            if (_disposed)
            {
                reply.SetCanceled();
                return reply.Task;
            }
            // The DLL's thread blocks on _gate until the reply is registered, so a reply that
            // arrives before Send returns is never lost; only an inline reply lands in _early.
            _sendingThread = Environment.CurrentManagedThreadId;
            try
            {
                id = _core.Send(command);
            }
            finally
            {
                _sendingThread = 0;
            }
            if (id == 0)
            {
                // Not registered: nothing will ever reply to number 0.
                var detail = new Dictionary<string, string> { ["detail"] = "cg_send returned 0" };
                now = new ReplyErr(_core.Tr("reason.internal", detail));
            }
            else
            {
                _pending[id] = reply;
                if (_early.Remove(id, out var early))
                {
                    now = early;
                }
            }
        }
        if (now is not null)
        {
            Complete(id, reply, now);
        }
        return reply.Task;
    }

    /// <summary>Completes a waiting command on the UI thread, or faults it if that cannot be posted.</summary>
    void Complete(ulong id, TaskCompletionSource<ReplyResult> waiting, ReplyResult result)
    {
        try
        {
            _dispatcher.Post(() =>
            {
                Forget(id, waiting);
                waiting.TrySetResult(result);
            });
        }
        catch (Exception e)
        {
            Forget(id, waiting);
            waiting.TrySetException(e);
        }
    }

    void Forget(ulong id, TaskCompletionSource<ReplyResult> waiting)
    {
        lock (_gate)
        {
            if (_pending.TryGetValue(id, out var current) && current == waiting)
            {
                _pending.Remove(id);
            }
        }
    }

    void OnEvent(CoreEvent ev)
    {
        if (ev is ReplyEvent r)
        {
            TaskCompletionSource<ReplyResult>? waiting;
            lock (_gate)
            {
                if (_disposed)
                {
                    return;
                }
                if (r.Id == 0 || !_pending.TryGetValue(r.Id, out waiting))
                {
                    if (_sendingThread == Environment.CurrentManagedThreadId)
                    {
                        _early[r.Id] = r.Result;
                    }
                    return;
                }
            }
            Complete(r.Id, waiting, r.Result);
            return;
        }
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }
        }
        _dispatcher.Post(() => Raise(ev));
    }

    void Raise(CoreEvent ev)
    {
        switch (ev)
        {
            case StateEvent e:
                State?.Invoke(e.View);
                break;
            case ServiceGoneEvent e:
                ServiceGone?.Invoke(e);
                break;
            case PhonePluggedEvent e:
                PhonePlugged?.Invoke(e.Serial);
                break;
            case SpeedProgressEvent e:
                SpeedProgress?.Invoke(e.View);
                break;
            case LeakProgressEvent e:
                LeakProgress?.Invoke(e.View);
                break;
            case PrefsEvent e:
                PrefsChanged?.Invoke(e.Prefs);
                break;
            case FaultEvent e:
                Fault?.Invoke(e.Message);
                break;
        }
    }

    /// <summary>Stops listening to the core and cancels every command still waiting for its reply.</summary>
    public void Dispose()
    {
        List<TaskCompletionSource<ReplyResult>> waiting;
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }
            _disposed = true;
            waiting = [.. _pending.Values];
            _pending.Clear();
            _early.Clear();
        }
        _core.EventReceived -= OnEvent;
        foreach (var command in waiting)
        {
            command.TrySetCanceled();
        }
    }
}
