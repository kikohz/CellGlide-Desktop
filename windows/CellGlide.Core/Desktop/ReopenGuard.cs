namespace CellGlide.Core.Desktop;

/// <summary>
/// The tray panel's reopen guard (cg-gui panel.rs REOPEN_GUARD): a tray click that comes this soon
/// after the panel closed on losing activation is the click that took the activation away, so it
/// must not open the panel again. The guard is used up by the first click after the close.
/// </summary>
public sealed class ReopenGuard(TimeSpan window)
{
    /// <summary>The guard the panel uses: 300 ms, as on Linux.</summary>
    public static readonly TimeSpan Default = TimeSpan.FromMilliseconds(300);

    TimeSpan? _closedAt;

    public ReopenGuard()
        : this(Default)
    {
    }

    /// <summary>The panel closed because it lost activation, at <paramref name="now"/>.</summary>
    public void ClosedOnDeactivation(TimeSpan now) => _closedAt = now;

    /// <summary>
    /// A tray click at <paramref name="now"/> on a closed panel: whether it may open it. Any earlier
    /// close is forgotten afterwards, so only the one click right after it is held back.
    /// </summary>
    public bool AllowsOpen(TimeSpan now)
    {
        var closedAt = _closedAt;
        _closedAt = null;
        return closedAt is not { } at || now - at >= window;
    }
}
