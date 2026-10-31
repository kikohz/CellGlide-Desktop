namespace CellGlide.Core.Desktop;

/// <summary>
/// Holds a signal that may come before anyone listens: the single-instance redirect can arrive
/// between registering the key (in Main) and the app being ready (OnLaunched). A signal with no
/// target is remembered and delivered once, when the target is attached.
/// </summary>
public sealed class ActivationLatch
{
    readonly Lock _gate = new();
    Action? _target;
    bool _pending;

    /// <summary>Delivers to the target, or remembers it until <see cref="Attach"/>. Any thread.</summary>
    public void Signal()
    {
        Action? target;
        lock (_gate)
        {
            target = _target;
            if (target is null)
            {
                _pending = true;
                return;
            }
        }
        target();
    }

    /// <summary>Sets the target; delivers a remembered signal to it at once.</summary>
    public void Attach(Action target)
    {
        bool pending;
        lock (_gate)
        {
            _target = target;
            pending = _pending;
            _pending = false;
        }
        if (pending)
        {
            target();
        }
    }
}
