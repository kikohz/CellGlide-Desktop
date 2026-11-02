namespace CellGlide.Core.Desktop;

/// <summary>Starts Apple Devices, the Microsoft Store app whose usbmuxd the service talks to.</summary>
public interface IAppleDevicesLauncher
{
    /// <summary>Starts its background part (usbmuxd) without a window; faults when that fails.</summary>
    Task StartInBackgroundAsync();

    /// <summary>Opens the Apple Devices window; faults when that fails.</summary>
    Task OpenAsync();
}

/// <summary>
/// Starts Apple Devices when the DLL says to (<c>OverviewView.StartAppleDevices</c>: Apple's device service
/// isn't running, and the service's doctor says the Apple Devices app is installed), and opens its window when
/// the user asks. The DLL decides when; this only keeps the pace: an episode lasts while the DLL keeps saying so,
/// its first attempt comes once it has lasted <see cref="FirstAfter"/>, and in one episode there is at most one
/// attempt per <see cref="Spacing"/> and at most <see cref="MaxAttempts"/> in all. Every attempt and its outcome
/// is logged; no failure escapes.
/// <para>
/// Why the wait: at sign-in Apple's own startup task starts usbmuxd, often within seconds and sometimes minutes
/// late; it gets its chance first, so the two don't race.
/// </para>
/// <para>Called on one thread (the UI's); the attempts themselves run where the launcher runs them.</para>
/// </summary>
public sealed class AppleDevicesStarter(IAppleDevicesLauncher launcher, Func<DateTimeOffset> now, Action<string> log)
{
    public static readonly TimeSpan FirstAfter = TimeSpan.FromSeconds(30);

    public static readonly TimeSpan Spacing = TimeSpan.FromSeconds(60);

    public const int MaxAttempts = 3;

    bool _episode;
    DateTimeOffset _since;
    int _attempts;
    DateTimeOffset _last;
    bool _gaveUp;
    Task _attempt = Task.CompletedTask;

    /// <summary>Attempts made in this episode.</summary>
    public int Attempts => _attempts;

    /// <summary>
    /// The DLL's flag as it is now; called whenever the state changes and every few seconds besides. Returns the
    /// attempt it started (already logged, never faulting), or a completed task when it started none.
    /// </summary>
    public Task Update(bool wanted)
    {
        if (!wanted)
        {
            if (_episode && _attempts > 0)
            {
                Log($"Apple's device service no longer needs starting (after {_attempts} attempt(s))");
            }
            _episode = false;
            _attempts = 0;
            _gaveUp = false;
            return Task.CompletedTask;
        }
        var at = now();
        if (!_episode)
        {
            _episode = true;
            _since = at;
            Log($"Apple's device service isn't running and Apple Devices is installed; first attempt in {FirstAfter.TotalSeconds:0} s");
        }
        if (!_attempt.IsCompleted || at - _since < FirstAfter || (_attempts > 0 && at - _last < Spacing))
        {
            return Task.CompletedTask;
        }
        if (_attempts >= MaxAttempts)
        {
            if (!_gaveUp)
            {
                _gaveUp = true;
                Log($"giving up after {MaxAttempts} attempts; the Open Apple Devices button stays");
            }
            return Task.CompletedTask;
        }
        _attempts++;
        _last = at;
        var n = _attempts;
        Log($"attempt {n}/{MaxAttempts}: starting Apple Devices in the background");
        return _attempt = Run(launcher.StartInBackgroundAsync, $"attempt {n}/{MaxAttempts}");
    }

    /// <summary>The user's button: opens the Apple Devices window (logged; never faulting).</summary>
    public Task OpenAsync()
    {
        Log("opening Apple Devices (the user's button)");
        return Run(launcher.OpenAsync, "opening Apple Devices");
    }

    async Task Run(Func<Task> action, string what)
    {
        try
        {
            await action();
            Log(what + ": done");
        }
        catch (Exception e)
        {
            Log($"{what}: failed: {e.GetType().Name} 0x{e.HResult:X8} {e.Message}");
        }
    }

    void Log(string line)
    {
        try
        {
            log("apple-devices: " + line);
        }
        catch (Exception)
        {
            // Logging never stops the app.
        }
    }
}
