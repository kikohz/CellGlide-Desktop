namespace CellGlide.Core.Desktop;

/// <summary>What the history page does with its refresh timer after a change of what is on screen.</summary>
public enum RefreshChange
{
    None,
    /// <summary>Start the timer, and ask for today now if the page follows today.</summary>
    Start,
    Stop,
}

/// <summary>
/// The history page refreshes today only while someone can see it: its page is the one shown, and the window
/// is visible and not minimised (cg-gui refreshes only while it draws). Closing the window hides it to the
/// tray without unloading the page, so the page's own Loaded / Unloaded is not enough.
/// </summary>
public sealed class HistoryRefreshGate
{
    /// <summary>The timer should be running.</summary>
    public bool Running { get; private set; }

    /// <summary>The new state of the page and its window; says whether the timer must start or stop.</summary>
    public RefreshChange Set(bool pageShown, bool windowVisible, bool minimised)
    {
        var wanted = pageShown && windowVisible && !minimised;
        if (wanted == Running)
        {
            return RefreshChange.None;
        }
        Running = wanted;
        return wanted ? RefreshChange.Start : RefreshChange.Stop;
    }
}
