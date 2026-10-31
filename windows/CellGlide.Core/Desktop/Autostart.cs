namespace CellGlide.Core.Desktop;

/// <summary>HKCU\...\CurrentVersion\Run, as far as autostart needs it.</summary>
public interface IRunKey
{
    string? Read(string name);

    void Write(string name, string value);

    void Delete(string name);
}

/// <summary>
/// HKCU\...\Explorer\StartupApproved\Run: where Windows records that the user turned an entry off
/// in Startup apps. Read only; the app never writes it.
/// </summary>
public interface IStartupApproved
{
    byte[]? Read(string name);
}

public enum AutostartState
{
    Off,
    On,

    /// <summary>Our value is there but the user turned it off in Windows' Startup apps; only Windows can turn it back on.</summary>
    DisabledBySystem,
}

/// <summary>Start in the tray after logging in: our own value in the Run key, and nothing else.</summary>
public sealed class Autostart(IRunKey run, IStartupApproved approved, string exePath)
{
    public const string ValueName = "CellGlide";

    /// <summary>The Run value for <paramref name="exePath"/>: the full path in quotes (it may hold spaces), then <c>--tray</c>.</summary>
    public static string CommandFor(string exePath) => "\"" + exePath + "\" --tray";

    /// <summary>
    /// Off without our value (a disabled byte Windows left behind is then not even read); disabled by
    /// the system when our value is there and StartupApproved's first byte has its lowest bit set;
    /// on when our value starts this executable (any case); off when it points elsewhere.
    /// </summary>
    public AutostartState Read()
    {
        var value = run.Read(ValueName);
        if (value is null)
        {
            return AutostartState.Off;
        }
        if (approved.Read(ValueName) is [var first, ..] && (first & 1) == 1)
        {
            return AutostartState.DisabledBySystem;
        }
        return string.Equals(value, CommandFor(exePath), StringComparison.OrdinalIgnoreCase) ? AutostartState.On : AutostartState.Off;
    }

    /// <summary>On writes (or rewrites) our value for this executable; off deletes it. Registry errors are passed on.</summary>
    public void Set(bool on)
    {
        if (on)
        {
            run.Write(ValueName, CommandFor(exePath));
        }
        else
        {
            run.Delete(ValueName);
        }
    }
}
