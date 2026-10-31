namespace CellGlide.Core.Desktop;

/// <summary>What the start-at-login switch shows after a click: the state read back, and why a write failed.</summary>
public sealed record AutostartChange(AutostartState State, string? Error);

/// <summary>
/// The settings page's start-at-login switch (cg-gui: "follows the file, not the click"): a click
/// writes or deletes our Run value, then the switch shows what reading it back says. When Windows'
/// Startup apps has turned us off nothing is written (only Windows can turn it back on). A registry
/// failure is passed on as its message, for <c>settings.autostart_failed</c>.
/// </summary>
public static class AutostartToggle
{
    public static AutostartChange Apply(Autostart autostart, bool on)
    {
        if (autostart.Read() == AutostartState.DisabledBySystem)
        {
            return new AutostartChange(AutostartState.DisabledBySystem, null);
        }
        string? error = null;
        try
        {
            autostart.Set(on);
        }
        catch (Exception e) when (e is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            error = e.Message;
        }
        return new AutostartChange(autostart.Read(), error);
    }
}

/// <summary>
/// A switch that shows one of the service's settings (connect automatically, strict DNS): it shows the
/// service's value; only a user's change to something else is sent, never while the last change is on
/// its way, and never without a state.
/// </summary>
public static class ServiceToggle
{
    public static bool ShouldSend(bool shown, bool? service, bool busy) =>
        !busy && service is { } value && shown != value;
}
