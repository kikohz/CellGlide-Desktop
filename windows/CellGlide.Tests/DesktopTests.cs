using System.Runtime.InteropServices;
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
    public async Task Quit_closes_the_hub_before_stopping_the_dll()
    {
        var core = new FakeCore(autoReply: new ReplyOk(new DoneValue()));
        var hub = new EventHub(core, new InlineDispatcher());
        var settings = new SettingsViewModel(hub);
        var speed = new SpeedViewModel(hub, settings);
        core.Raise(Samples.Event("speed_progress_download.json"));
        await new AppLifetime(hub, core, speed, new LeakViewModel(hub)).QuitAsync(TimeSpan.FromSeconds(2));
        // By cg_shutdown the hub has let go of the core's events, so no callback reaches C# code
        // that could wait on the UI thread, and the hub is closed: later commands are cancelled.
        Assert.False(core.ListenedAtDispose);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => hub.SendAsync(new SubscribeCommand()));
    }

    [Fact]
    public void Core_start_failure_from_a_broken_install_is_null()
    {
        Assert.Null(CoreStart.TryStart(() => throw new DllNotFoundException()));
        Assert.Null(CoreStart.TryStart(() => throw new EntryPointNotFoundException()));
        Assert.Null(CoreStart.TryStart(() => throw new BadImageFormatException()));
        Assert.Null(CoreStart.TryStart(() => throw new InvalidOperationException("cg_init returned -1")));
        var core = new FakeCore();
        Assert.Same(core, CoreStart.TryStart(() => core));
        // Anything else is a bug, not an installation problem.
        Assert.Throws<ArgumentException>(() => CoreStart.TryStart(() => throw new ArgumentException()));
    }

    [Fact]
    public void Core_start_failure_when_the_dll_is_not_there()
    {
        // The real path: a P/Invoke into a library that is not there.
        Assert.Null(CoreStart.TryStart(() =>
        {
            _ = MissingLibraryCall();
            return new FakeCore();
        }));
    }

    [DllImport("cellglide_missing_for_tests.dll")]
    static extern int MissingLibraryCall();

    [Fact]
    public void An_activation_before_the_app_is_ready_is_delivered_once_on_attach()
    {
        var latch = new ActivationLatch();
        latch.Signal();
        latch.Signal();
        var seen = 0;
        latch.Attach(() => seen++);
        Assert.Equal(1, seen);
        latch.Signal();
        Assert.Equal(2, seen);
    }

    [Fact]
    public void Without_an_early_activation_attach_delivers_nothing()
    {
        var latch = new ActivationLatch();
        var seen = 0;
        latch.Attach(() => seen++);
        Assert.Equal(0, seen);
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

    // ---- Task 5: the tray icon file, its GUID, where the panel goes, chart geometry ----

    static readonly PixelRect Screen = new(0, 0, 1920, 1032);

    [Fact]
    public void Panel_sits_above_a_bottom_taskbar_icon() =>
        Assert.Equal(new PixelPoint(1542, 560), PanelPlacement.Place(new PixelRect(1700, 1040, 1724, 1072), Screen, TaskbarEdge.Bottom, 340, 460, 12));

    [Fact]
    public void Panel_is_clamped_inside_the_work_area() =>
        Assert.Equal(new PixelPoint(1568, 560), PanelPlacement.Place(new PixelRect(1880, 1040, 1904, 1072), Screen, TaskbarEdge.Bottom, 340, 460, 12));

    [Fact]
    public void Panel_without_an_icon_goes_to_the_corner() =>
        Assert.Equal(new PixelPoint(1568, 560), PanelPlacement.Place(null, Screen, TaskbarEdge.Bottom, 340, 460, 12));

    [Fact]
    public void Panel_beside_a_top_or_left_taskbar()
    {
        Assert.Equal(new PixelPoint(1542, 60), PanelPlacement.Place(new PixelRect(1700, 8, 1724, 40), new PixelRect(0, 48, 1920, 1080), TaskbarEdge.Top, 340, 460, 12));
        Assert.Equal(new PixelPoint(60, 608), PanelPlacement.Place(new PixelRect(8, 900, 40, 924), new PixelRect(48, 0, 1920, 1080), TaskbarEdge.Left, 340, 460, 12));
    }

    [Fact]
    public void Panel_beside_a_right_taskbar_and_in_the_other_corners()
    {
        var work = new PixelRect(0, 0, 1872, 1080);
        Assert.Equal(new PixelPoint(1520, 422), PanelPlacement.Place(new PixelRect(1880, 640, 1912, 664), work, TaskbarEdge.Right, 340, 460, 12));
        // No icon: the corner at that edge's end (bottom right; bottom left for a left taskbar).
        Assert.Equal(new PixelPoint(1520, 608), PanelPlacement.Place(null, work, TaskbarEdge.Right, 340, 460, 12));
        Assert.Equal(new PixelPoint(1520, 12), PanelPlacement.Place(null, work, TaskbarEdge.Top, 340, 460, 12));
        Assert.Equal(new PixelPoint(60, 608), PanelPlacement.Place(null, new PixelRect(48, 0, 1920, 1080), TaskbarEdge.Left, 340, 460, 12));
    }

    [Fact]
    public void Panel_on_a_left_monitor_with_negative_coordinates() =>
        Assert.Equal(new PixelPoint(-352, 568), PanelPlacement.Place(null, new PixelRect(-1920, 0, 0, 1040), TaskbarEdge.Bottom, 340, 460, 12));

    [Fact]
    public void Panel_taller_than_the_work_area_starts_at_the_top() =>
        Assert.Equal(0, PanelPlacement.Place(null, new PixelRect(0, 0, 800, 400), TaskbarEdge.Bottom, 340, 460, 12).Y);

    [Fact]
    public void Panel_wider_than_the_work_area_starts_at_the_left() =>
        Assert.Equal(-100, PanelPlacement.Place(new PixelRect(0, 500, 20, 520), new PixelRect(-100, 0, 100, 1000), TaskbarEdge.Bottom, 340, 460, 12).X);

    [Fact]
    public void Tray_icon_follows_service_gone_chrome()
    {
        var gone = Samples.Chrome();                                  // service_gone_stopped.json
        Assert.Equal(@"Assets\Tray\cellglide-idle-dark.ico", TrayIcons.FileFor(gone.Tray, lightTaskbar: false));
        var connected = Samples.Overview().Chrome.Tray;               // active
        Assert.Equal(@"Assets\Tray\cellglide-active-light.ico", TrayIcons.FileFor(connected with { Alert = false }, lightTaskbar: true));
        Assert.Equal(@"Assets\Tray\cellglide-active-alert-dark.ico", TrayIcons.FileFor(connected with { Alert = true }, lightTaskbar: false));
    }

    [Fact]
    public void Every_tray_icon_file_is_in_the_app()
    {
        var assets = Path.Combine(AppSources.AppDir, "Assets", "Tray");
        foreach (var active in new[] { false, true })
        {
            foreach (var alert in new[] { false, true })
            {
                foreach (var light in new[] { false, true })
                {
                    var file = TrayIcons.FileFor(new TrayLook(active, alert, false, ""), light);
                    Assert.True(File.Exists(Path.Combine(AppSources.AppDir, file)), file);
                }
            }
        }
        Assert.Equal(8, Directory.GetFiles(assets, "*.ico").Length);
        Assert.Equal(@"Assets\Tray\cellglide-alert-light.ico", TrayIcons.FileFor(new TrayLook(false, true, true, ""), lightTaskbar: true));
    }

    [Fact]
    public void Tray_menu_is_rebuilt_only_when_its_items_change()
    {
        // Each read of a sample is a new list with new records, as each state event is.
        var connected = Samples.Overview().Chrome.Menu;
        Assert.True(TrayMenu.Same(connected, Samples.Overview().Chrome.Menu));
        Assert.False(TrayMenu.Same(connected, Samples.Chrome().Menu));            // Disconnect -> Connect
        Assert.False(TrayMenu.Same(null, connected));                             // nothing built yet
        Assert.False(TrayMenu.Same(connected, null));
        var disabled = connected.Select(i => i.Kind == MenuKind.Toggle ? i with { Enabled = false } : i).ToList();
        Assert.False(TrayMenu.Same(connected, disabled));
        Assert.False(TrayMenu.Same(connected, connected.Take(5).ToList()));
        Assert.False(TrayMenu.Same(connected, connected.Reverse().ToList()));
    }

    [Fact]
    public void The_click_that_took_the_activation_does_not_reopen_the_panel()
    {
        var t0 = TimeSpan.FromSeconds(100);
        var guard = new ReopenGuard();
        // Nothing closed it: a click opens.
        Assert.True(guard.AllowsOpen(t0));
        // Closed on losing activation; the click 200 ms later is the one that took it.
        guard.ClosedOnDeactivation(t0);
        Assert.False(guard.AllowsOpen(t0 + TimeSpan.FromMilliseconds(200)));
        // The guard is used up: the next click opens.
        Assert.True(guard.AllowsOpen(t0 + TimeSpan.FromMilliseconds(250)));
        // A click well after the close opens.
        guard.ClosedOnDeactivation(t0);
        Assert.True(guard.AllowsOpen(t0 + TimeSpan.FromMilliseconds(400)));
        // Exactly at the limit counts as after it.
        guard.ClosedOnDeactivation(t0);
        Assert.True(guard.AllowsOpen(t0 + ReopenGuard.Default));
        Assert.Equal(TimeSpan.FromMilliseconds(300), ReopenGuard.Default);
    }

    [Fact]
    public void Tray_guid_is_stable_per_path()
    {
        var a = TrayIdentity.GuidFor(@"C:\Program Files\CellGlide\app\CellGlide.exe");
        Assert.Equal(a, TrayIdentity.GuidFor(@"c:\program files\cellglide\APP\CellGlide.exe"));
        Assert.NotEqual(a, TrayIdentity.GuidFor(@"D:\dev\CellGlide.exe"));
        Assert.Equal('5', a.ToString()[14]);
        // Python: uuid.uuid5(UUID('6f1d3a52-8c2e-4b7a-9e43-2d5c0b8f71a4'), r'C:\PROGRAM FILES\CELLGLIDE\APP\CELLGLIDE.EXE')
        Assert.Equal(new Guid("8f470192-ae5d-5fd6-b76e-2489ab95fcd2"), a);
        // The same file named through ".." is the same icon.
        Assert.Equal(a, TrayIdentity.GuidFor(@"C:\Program Files\CellGlide\x\..\app\CellGlide.exe"));
    }

    [Fact]
    public void Geometry_flips_into_the_box()
    {
        var pts = Geometry.Plot([new CurvePoint(0, 0), new CurvePoint(1, 1)], 200, 50);
        Assert.Equal(new[] { new PlotPoint(0, 50), new PlotPoint(200, 0) }, pts);
        Assert.Equal(new StripSpan(150, 1), Geometry.Strip(new StripCell(500, 0.5f, Tone.Green, 0.6f), 300));
    }

    [Fact]
    public void Geometry_of_a_trail_run_strips_and_ticks()
    {
        TrailPoint[] run = [new(0.5f, 1, 0.25f), new(1, 0, 0.5f)];
        Assert.Equal(new[] { new PlotPoint(50, 0), new PlotPoint(100, 40) }, Geometry.Plot(run, upload: false, 100, 40));
        Assert.Equal(new[] { new PlotPoint(50, 30), new PlotPoint(100, 20) }, Geometry.Plot(run, upload: true, 100, 40));
        Assert.Equal(new StripSpan(0, 600), Geometry.Strip(new StripCell(0, 1000, Tone.Gray, 1), 600));
        Assert.Equal(150, Geometry.Tick(0.25, 600));
    }

    [Fact]
    public void Badges_come_from_the_chrome_for_their_sections()
    {
        var b = new BadgesView(Badge.Warning, Badge.Critical, Badge.Warning);
        Assert.Equal(Badge.Warning, SidebarBadges.For(b, Section.Overview));
        Assert.Equal(Badge.Critical, SidebarBadges.For(b, Section.History));
        Assert.Equal(Badge.Warning, SidebarBadges.For(b, Section.Settings));
        Assert.Equal(Badge.None, SidebarBadges.For(b, Section.SpeedTest));
    }

    [Fact]
    public void Sections_without_a_field_never_have_a_badge()
    {
        var all = new BadgesView(Badge.Critical, Badge.Critical, Badge.Critical);
        foreach (var section in new[] { Section.SpeedTest, Section.DnsLeak, Section.Diagnostics, Section.About })
        {
            Assert.Equal(Badge.None, SidebarBadges.For(all, section));
        }
        var none = new BadgesView(Badge.None, Badge.None, Badge.None);
        Assert.All(Enum.GetValues<Section>(), section => Assert.Equal(Badge.None, SidebarBadges.For(none, section)));
    }

    [Fact]
    public void Sidebar_is_rebuilt_only_when_its_groups_change()
    {
        // Each read of a sample is a new list with new records, as each state event is.
        var shown = Samples.Overview().Chrome.Sidebar;
        Assert.True(SidebarLayout.Same(shown, Samples.Overview().Chrome.Sidebar));
        // The service-gone chrome has the same sidebar: only the dots differ.
        Assert.True(SidebarLayout.Same(shown, Samples.Chrome().Sidebar));
        Assert.False(SidebarLayout.Same(null, shown));                          // nothing built yet
        Assert.False(SidebarLayout.Same(shown, null));
        var renamed = shown.Select((g, i) => i == 1 ? g with { Heading = "x" } : g).ToList();
        Assert.False(SidebarLayout.Same(shown, renamed));
        var retitled = shown.Select((g, i) => i == 0
            ? g with { Sections = g.Sections.Select(s => s with { Title = s.Title + "!" }).ToList() }
            : g).ToList();
        Assert.False(SidebarLayout.Same(shown, retitled));
        var fewer = shown.Select((g, i) => i == 3 ? g with { Sections = g.Sections.Take(1).ToList() } : g).ToList();
        Assert.False(SidebarLayout.Same(shown, fewer));
        Assert.False(SidebarLayout.Same(shown, shown.Take(3).ToList()));
    }

    [Fact]
    public void Sample_sidebar_is_four_groups_of_seven_sections()
    {
        // What the main window builds its NavigationView from: the DLL's groups, in its order.
        var sidebar = Samples.Overview().Chrome.Sidebar;
        Assert.Equal(4, sidebar.Count);
        Assert.Null(sidebar[0].Heading);
        Assert.All(sidebar.Skip(1), g => Assert.False(string.IsNullOrEmpty(g.Heading)));
        Assert.Equal(Enum.GetValues<Section>(), sidebar.SelectMany(g => g.Sections).Select(s => s.Section));
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
