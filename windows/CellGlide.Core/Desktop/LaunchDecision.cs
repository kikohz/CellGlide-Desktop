namespace CellGlide.Core.Desktop;

/// <summary>What a starting process does.</summary>
public enum Launch
{
    /// <summary>Become the app.</summary>
    Run,

    /// <summary>Hand the activation to the running app (which shows its window), then exit.</summary>
    Redirect,

    /// <summary>Exit at once: the app is already running.</summary>
    Exit,
}

/// <summary>
/// Single instance, the host's own rule (argv plus AppInstance). It behaves like the Linux host's
/// (cg-gui <c>single::decide</c>), which is that host's own code: nothing shared is copied.
/// </summary>
public static class LaunchDecision
{
    /// <summary>
    /// The first instance runs. A later one started with <c>--tray</c> (login while the app is open)
    /// just exits; any other later one (the user opened it again) brings the running app forward.
    /// </summary>
    public static Launch Decide(bool firstInstance, bool tray) =>
        firstInstance ? Launch.Run : tray ? Launch.Exit : Launch.Redirect;
}
