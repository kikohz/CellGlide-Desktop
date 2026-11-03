namespace CellGlide.Core.Desktop;

/// <summary>Starts Apple Devices, the Microsoft Store app whose usbmuxd the service talks to.</summary>
public interface IAppleDevicesLauncher
{
    /// <summary>Starts its background part (usbmuxd) without a window; faults when that fails.</summary>
    Task StartInBackgroundAsync();

    /// <summary>Opens the Apple Devices window; faults when that fails.</summary>
    Task OpenAsync();

    /// <summary>Whether something listens on usbmuxd's port (127.0.0.1:27015) right now.</summary>
    bool UsbmuxdListening();
}

/// <summary>
/// Starts Apple Devices when the DLL says to (<c>OverviewView.StartAppleDevices</c>: Apple's device service
/// isn't running, and the service's doctor says the Apple Devices app is installed), and opens its window when
/// the user asks. The DLL decides when; this only keeps the pace:
/// <list type="bullet">
/// <item>an episode lasts while the DLL keeps saying so; its first attempt comes once it has lasted
/// <see cref="FirstAfter"/>, and it makes at most <see cref="MaxAttempts"/>;</item>
/// <item>across episodes too, attempts are at least <see cref="Spacing"/> apart;</item>
/// <item>and there are at most <see cref="MaxPerWindow"/> in any <see cref="Window"/>, so an usbmuxd that keeps
/// ending (every episode then ends after one attempt) can't make the app start PowerShell without end: the
/// button stays.</item>
/// </list>
/// Every attempt and its outcome is logged; no failure escapes.
/// <para>
/// Why the wait: at sign-in Apple's own startup task starts usbmuxd, often within seconds and sometimes minutes
/// late; it gets its chance first, so the two don't race.
/// </para>
/// <para>Called on one thread (the UI's); the attempts themselves run where the launcher runs them.</para>
/// </summary>
public sealed class AppleDevicesStarter(
    IAppleDevicesLauncher launcher,
    Func<DateTimeOffset> now,
    Action<string> log,
    Func<TimeSpan, Task>? delay = null)
{
    public static readonly TimeSpan FirstAfter = TimeSpan.FromSeconds(30);

    public static readonly TimeSpan Spacing = TimeSpan.FromSeconds(60);

    public const int MaxAttempts = 3;

    public static readonly TimeSpan Window = TimeSpan.FromHours(1);

    public const int MaxPerWindow = 6;

    /// <summary>How long the button waits for usbmuxd after starting it, before it opens the window.</summary>
    public static readonly TimeSpan OpenWait = TimeSpan.FromSeconds(10);

    static readonly TimeSpan OpenPoll = TimeSpan.FromMilliseconds(500);

    readonly Func<TimeSpan, Task> _delay = delay ?? (d => Task.Delay(d));

    /// <summary>When the automatic attempts were made, the latest last; kept across episodes, pruned to <see cref="Window"/>.</summary>
    readonly List<DateTimeOffset> _made = [];

    bool _episode;
    DateTimeOffset _since;
    int _attempts;
    bool _gaveUp;
    bool _capped;
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
            Log($"Apple's device service isn't running and Apple Devices is installed; first attempt in {FirstAfter.TotalSeconds:0} s at the earliest");
        }
        _made.RemoveAll(t => at - t >= Window);
        var spaced = _made.Count == 0 || at - _made[^1] >= Spacing;
        if (!_attempt.IsCompleted || at - _since < FirstAfter || !spaced)
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
        if (_made.Count >= MaxPerWindow)
        {
            if (!_capped)
            {
                _capped = true;
                Log($"{MaxPerWindow} attempts in the last {Window.TotalMinutes:0} minutes; the next no sooner than {(_made[0] + Window).ToLocalTime():HH:mm:ss}");
            }
            return Task.CompletedTask;
        }
        _capped = false;
        _attempts++;
        _made.Add(at);
        var n = _attempts;
        Log($"attempt {n}/{MaxAttempts}: starting Apple Devices in the background");
        return _attempt = Run(launcher.StartInBackgroundAsync, $"attempt {n}/{MaxAttempts}");
    }

    /// <summary>
    /// The user's button: when nothing listens on usbmuxd's port, first starts the background part as the automatic
    /// attempts do (so the window's own agent doesn't start usbmuxd, the way that so often left it blind to the
    /// iPhone) and waits up to <see cref="OpenWait"/> for it; then opens the Apple Devices window. Not paced: the
    /// user asked. Logged; never faulting.
    /// </summary>
    public async Task OpenAsync()
    {
        Log("opening Apple Devices (the user's button)");
        if (!Listening())
        {
            Log("usbmuxd isn't listening: starting it first");
            await Run(launcher.StartInBackgroundAsync, "starting before opening");
            var waited = TimeSpan.Zero;
            while (!Listening() && waited < OpenWait)
            {
                await _delay(OpenPoll);
                waited += OpenPoll;
            }
            Log(Listening() ? "usbmuxd is listening" : $"usbmuxd still isn't listening after {OpenWait.TotalSeconds:0} s; opening anyway");
        }
        await Run(launcher.OpenAsync, "opening Apple Devices");
    }

    bool Listening()
    {
        try
        {
            return launcher.UsbmuxdListening();
        }
        catch (Exception e)
        {
            Log($"couldn't tell whether usbmuxd listens: {e.GetType().Name} {e.Message}");
            return false;
        }
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
