using CellGlide.Core.Models;

namespace CellGlide.Core.Dispatch;

/// <summary>
/// Takes every event off the DLL's thread and raises it on the UI thread, through
/// <see cref="IUiDispatcher.Post"/>; matches replies to the commands sent with <see cref="SendAsync"/>.
/// Nothing here waits on the DLL's thread, so the DLL may be started or stopped from any handler.
/// </summary>
public sealed class EventHub : IDisposable
{
    readonly ICore _core;
    readonly IUiDispatcher _dispatcher;
    readonly Lock _gate = new();
    readonly Dictionary<ulong, TaskCompletionSource<ReplyResult>> _pending = [];
    // Replies that came back on the sending thread before Send returned (a core that answers inline).
    readonly Dictionary<ulong, ReplyResult> _early = [];
    int _sendingThread;

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

    /// <summary>Sends <paramref name="command"/>; completes with its own reply, on the UI thread.</summary>
    public Task<ReplyResult> SendAsync(Command command)
    {
        var reply = new TaskCompletionSource<ReplyResult>(TaskCreationOptions.RunContinuationsAsynchronously);
        lock (_gate)
        {
            // The DLL's thread blocks on _gate until the reply is registered, so a reply that
            // arrives before Send returns is never lost; only an inline reply lands in _early.
            _sendingThread = Environment.CurrentManagedThreadId;
            ulong id;
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
                var detail = new Dictionary<string, string> { ["detail"] = "cg_send returned 0" };
                reply.SetResult(new ReplyErr(_core.Tr("reason.internal", detail)));
            }
            else if (_early.Remove(id, out var early))
            {
                _dispatcher.Post(() => reply.TrySetResult(early));
            }
            else
            {
                _pending[id] = reply;
            }
        }
        return reply.Task;
    }

    void OnEvent(CoreEvent ev)
    {
        if (ev is ReplyEvent r)
        {
            TaskCompletionSource<ReplyResult>? waiting;
            lock (_gate)
            {
                if (!_pending.Remove(r.Id, out waiting))
                {
                    if (_sendingThread == Environment.CurrentManagedThreadId)
                    {
                        _early[r.Id] = r.Result;
                    }
                    return;
                }
            }
            _dispatcher.Post(() => waiting.TrySetResult(r.Result));
            return;
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

    /// <summary>Stops listening to the core. Commands still waiting are not completed.</summary>
    public void Dispose() => _core.EventReceived -= OnEvent;
}
