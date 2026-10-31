using System.Runtime.Versioning;
using CellGlide.Core.Desktop;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;

namespace CellGlide.Tests;

/// <summary>The app host's logic that needs no window: launch options, autostart, the quit order.</summary>
public class DesktopTests
{
    [Fact]
    public void Launch_options_read_tray_and_small_caps()
    {
        Assert.Equal(new LaunchOptions(false, null), LaunchOptions.Parse([], null));
        Assert.True(LaunchOptions.Parse(["--TRAY"], null).Tray);
        Assert.Equal(new SpeedCaps(10_000_000, 5_000_000, 5), LaunchOptions.Parse([], "small").SpeedCaps);
        Assert.Null(LaunchOptions.Parse([], "full").SpeedCaps);
    }

    [Fact]
    public void Launch_options_ignore_other_arguments()
    {
        Assert.Equal(new LaunchOptions(true, new SpeedCaps(10_000_000, 5_000_000, 5)), LaunchOptions.Parse(["x", "--tray", "-tray"], "SMALL"));
        Assert.False(LaunchOptions.Parse(["tray", "--tray=1"], null).Tray);
    }

    [Fact]
    public void Launch_decision_matches_linux()
    {
        Assert.Equal(Launch.Run, LaunchDecision.Decide(firstInstance: true, tray: true));
        Assert.Equal(Launch.Exit, LaunchDecision.Decide(firstInstance: false, tray: true));
        Assert.Equal(Launch.Redirect, LaunchDecision.Decide(firstInstance: false, tray: false));
    }

    const string Exe = @"C:\Program Files\CellGlide\app\CellGlide.exe";

    [Fact]
    public void Autostart_command_quotes_the_path() =>
        Assert.Equal("\"C:\\Program Files\\CellGlide\\app\\CellGlide.exe\" --tray", Autostart.CommandFor(Exe));

    [Fact]
    public void Autostart_points_elsewhere_reads_off()
    {
        var run = new FakeRunKey { ["CellGlide"] = "\"D:\\old\\CellGlide.exe\" --tray" };
        var a = new Autostart(run, new FakeApproved(), Exe);
        Assert.Equal(AutostartState.Off, a.Read());
        a.Set(true);
        Assert.Equal(Autostart.CommandFor(Exe), run["CellGlide"]);
        Assert.Equal(AutostartState.On, a.Read());
    }

    [Fact]
    public void Autostart_matches_the_path_ignoring_case()
    {
        var run = new FakeRunKey { ["CellGlide"] = Autostart.CommandFor(Exe.ToUpperInvariant()) };
        Assert.Equal(AutostartState.On, new Autostart(run, new FakeApproved(), Exe).Read());
    }

    [Fact]
    public void Autostart_disabled_by_system()
    {
        var run = new FakeRunKey { ["CellGlide"] = Autostart.CommandFor(Exe) };
        Assert.Equal(AutostartState.DisabledBySystem, new Autostart(run, new FakeApproved { ["CellGlide"] = [0x03, 0, 0, 0] }, Exe).Read());
        Assert.Equal(AutostartState.On, new Autostart(run, new FakeApproved { ["CellGlide"] = [0x02, 0, 0, 0] }, Exe).Read());
        Assert.Equal(AutostartState.On, new Autostart(run, new FakeApproved { ["CellGlide"] = [] }, Exe).Read());
    }

    [Fact]
    public void Autostart_stale_disabled_byte_without_our_value_reads_off()
    {
        var run = new FakeRunKey();
        var approved = new FakeApproved { ["CellGlide"] = [0x03, 0, 0, 0] };
        var a = new Autostart(run, approved, Exe);
        Assert.Equal(AutostartState.Off, a.Read());                  // off, and can be turned on
        Assert.Equal(0, approved.Reads);                             // without our value it is not even read
        a.Set(true);
        Assert.Equal(Autostart.CommandFor(Exe), run["CellGlide"]);
        // Once our value is there, the system's choice counts again (it is the user's, in Startup apps).
        Assert.Equal(AutostartState.DisabledBySystem, a.Read());
        Assert.Equal(1, approved.Reads);
    }

    [Fact]
    public void Autostart_off_deletes_only_our_value()
    {
        var run = new FakeRunKey { ["CellGlide"] = Autostart.CommandFor(Exe), ["Other"] = "x" };
        new Autostart(run, new FakeApproved(), Exe).Set(false);
        Assert.False(run.Contains("CellGlide"));
        Assert.Equal("x", run["Other"]);
    }

    [Fact]
    public void Autostart_passes_registry_failures_on()
    {
        var a = new Autostart(new FakeRunKey { Fail = true }, new FakeApproved(), Exe);
        Assert.Throws<UnauthorizedAccessException>(() => a.Set(true));
        Assert.Throws<UnauthorizedAccessException>(() => a.Set(false));
    }

    [Fact]
    [SupportedOSPlatform("windows")]   // the throwing calls below are in lambdas, out of reach of the guard
    public void Registry_run_key_round_trips_in_a_scratch_key()
    {
        if (!OperatingSystem.IsWindows())
        {
            return;
        }
        var path = @"Software\CellGlide-Tests-" + Guid.NewGuid().ToString("N");
        try
        {
            var key = new RegistryRunKey(path);
            Assert.Null(key.Read("CellGlide"));
            key.Delete("CellGlide");                                 // nothing there: no error
            key.Write("CellGlide", "v");
            Assert.Equal("v", key.Read("CellGlide"));
            using (var k = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(path)!)
            {
                Assert.Equal(Microsoft.Win32.RegistryValueKind.String, k.GetValueKind("CellGlide"));
            }
            key.Delete("CellGlide");
            Assert.Null(key.Read("CellGlide"));
            // Only our own value is ever written or deleted.
            Assert.Throws<ArgumentException>(() => key.Write("Other", "v"));
            Assert.Throws<ArgumentException>(() => key.Delete("Other"));
        }
        finally
        {
            Microsoft.Win32.Registry.CurrentUser.DeleteSubKeyTree(path, throwOnMissingSubKey: false);
        }
    }

    [Fact]
    public void Registry_startup_approved_reads_a_scratch_key()
    {
        if (!OperatingSystem.IsWindows())
        {
            return;
        }
        var path = @"Software\CellGlide-Tests-" + Guid.NewGuid().ToString("N");
        try
        {
            Assert.Null(new RegistryStartupApproved(path).Read("CellGlide"));   // no key at all
            using (var k = Microsoft.Win32.Registry.CurrentUser.CreateSubKey(path))
            {
                k.SetValue("CellGlide", new byte[] { 0x03, 0, 0, 0 }, Microsoft.Win32.RegistryValueKind.Binary);
            }
            Assert.Equal(new byte[] { 0x03, 0, 0, 0 }, new RegistryStartupApproved(path).Read("CellGlide"));
            Assert.Null(new RegistryStartupApproved(path).Read("Other"));
        }
        finally
        {
            Microsoft.Win32.Registry.CurrentUser.DeleteSubKeyTree(path, throwOnMissingSubKey: false);
        }
    }

    [Fact]
    public void Registry_startup_approved_has_no_way_to_write()
    {
        var writers = typeof(RegistryStartupApproved).GetMethods()
            .Where(m => m.DeclaringType == typeof(RegistryStartupApproved))
            .Select(m => m.Name);
        Assert.Equal(["Read"], writers);
    }

    [Fact]
    public async Task Quit_cancels_running_tests_then_shuts_down()
    {
        var core = new FakeCore(autoReply: new ReplyOk(new DoneValue()));
        var hub = new EventHub(core, new InlineDispatcher());
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        var leak = new LeakViewModel(hub);
        core.Raise(Samples.Event("speed_progress_download.json"));      // Running = true
        core.Raise(Samples.Event("leak_progress_running.json"));
        await new AppLifetime(hub, core, speed, leak).QuitAsync(TimeSpan.FromSeconds(2));
        Assert.Collection(core.Sent, c => Assert.IsType<SpeedCancelCommand>(c), c => Assert.IsType<LeakCancelCommand>(c));
        Assert.Equal(2, core.SentCountAtDispose);
    }

    [Fact]
    public async Task Quit_with_nothing_running_sends_nothing()
    {
        var core = new FakeCore(autoReply: new ReplyOk(new DoneValue()));
        var hub = new EventHub(core, new InlineDispatcher());
        var settings = new SettingsViewModel(hub);
        await new AppLifetime(hub, core, new SpeedViewModel(hub, settings), new LeakViewModel(hub)).QuitAsync(TimeSpan.FromSeconds(2));
        Assert.Empty(core.Sent);
        Assert.True(core.Disposed);
        // The hub is closed too: a late command is cancelled, not sent.
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => hub.SendAsync(new SubscribeCommand()));
    }

    [Fact]
    public async Task Quit_finishes_when_no_reply_comes()
    {
        var core = new FakeCore();                                     // never replies
        var hub = new EventHub(core, new InlineDispatcher());
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        core.Raise(Samples.Event("speed_progress_download.json"));
        var quit = new AppLifetime(hub, core, speed, new LeakViewModel(hub)).QuitAsync(TimeSpan.FromMilliseconds(200));
        await quit.WaitAsync(TimeSpan.FromSeconds(3), TestContext.Current.CancellationToken);
        Assert.True(core.Disposed);
    }

    [Fact]
    public async Task Quit_twice_is_one_quit()
    {
        var core = new FakeCore(autoReply: new ReplyOk(new DoneValue()));
        var hub = new EventHub(core, new InlineDispatcher());
        var settings = new SettingsViewModel(hub);
        var life = new AppLifetime(hub, core, new SpeedViewModel(hub, settings), new LeakViewModel(hub));
        Assert.Same(life.QuitAsync(TimeSpan.FromSeconds(1)), life.QuitAsync(TimeSpan.FromSeconds(1)));
        await life.QuitAsync(TimeSpan.FromSeconds(1));
        Assert.Equal(1, core.DisposeCount);
    }
}

/// <summary>A Run key in memory.</summary>
sealed class FakeRunKey : IRunKey
{
    readonly Dictionary<string, string> _values = [];

    /// <summary>Every write and delete throws, as the registry does when access is denied.</summary>
    public bool Fail { get; init; }

    public string this[string name]
    {
        get => _values[name];
        set => _values[name] = value;
    }

    public bool Contains(string name) => _values.ContainsKey(name);

    public string? Read(string name) => _values.GetValueOrDefault(name);

    public void Write(string name, string value)
    {
        if (Fail)
        {
            throw new UnauthorizedAccessException();
        }
        _values[name] = value;
    }

    public void Delete(string name)
    {
        if (Fail)
        {
            throw new UnauthorizedAccessException();
        }
        _values.Remove(name);
    }
}

/// <summary>StartupApproved in memory; counts reads.</summary>
sealed class FakeApproved : IStartupApproved
{
    readonly Dictionary<string, byte[]> _values = [];

    public int Reads { get; private set; }

    public byte[] this[string name]
    {
        get => _values[name];
        set => _values[name] = value;
    }

    public bool Contains(string name) => _values.ContainsKey(name);

    public byte[]? Read(string name)
    {
        Reads++;
        return _values.GetValueOrDefault(name);
    }
}
