using System.Diagnostics;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Desktop;
using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.Native;
using CellGlide.Core.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace CellGlide.App;

/// <summary>
/// The host: starts the DLL in the system's language with its events on the UI thread, builds the
/// view models, shows the main window unless started with <c>--tray</c>, brings it forward when the
/// user starts the app again, opens it when the DLL says a phone was plugged in (only while hidden,
/// and if the preference allows), and quits in order.
/// </summary>
public partial class App : Application
{
    /// <summary>How long quitting waits for a running test to confirm its cancel.</summary>
    static readonly TimeSpan CancelWait = TimeSpan.FromSeconds(2);

    readonly LaunchOptions _options;
    DispatcherQueue? _queue;
    AppLifetime? _lifetime;
    MainWindow? _window;

    public App(LaunchOptions options)
    {
        _options = options;
        InitializeComponent();
    }

    /// <summary>The running app.</summary>
    public static new App Current => (App)Application.Current;

    public EventHub Hub { get; private set; } = null!;

    public ShellViewModel Shell { get; private set; } = null!;

    public SettingsViewModel Settings { get; private set; } = null!;

    public SpeedViewModel Speed { get; private set; } = null!;

    public LeakViewModel Leak { get; private set; } = null!;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _queue = DispatcherQueue.GetForCurrentThread();
        var core = NativeCore.Start("");
        Hub = new EventHub(core, new DispatcherQueueDispatcher(_queue));
        L.Hub = Hub;
        Shell = new ShellViewModel(Hub);
        Settings = new SettingsViewModel(Hub);
        Speed = new SpeedViewModel(Hub, Settings) { Caps = _options.SpeedCaps };
        Leak = new LeakViewModel(Hub);
        _lifetime = new AppLifetime(Hub, core, Speed, Leak);

        Hub.PhonePlugged += OnPhonePlugged;
        Hub.Fault += message => Debug.WriteLine("CellGlide: fault: " + message);
        // A later start of the app (without --tray) lands here, on a background thread.
        AppInstance.GetCurrent().Activated += (_, _) => _queue.TryEnqueue(() => ShowWindow(null));

        _ = Shell.SubscribeAsync();
        _ = Settings.LoadAsync();
        if (!_options.Tray)
        {
            ShowWindow(null);
        }
    }

    /// <summary>
    /// The DLL decided a phone was plugged in (that event is the whole decision); open the overview
    /// only if the window is hidden and the user wants it opened. Nothing else is decided here.
    /// </summary>
    void OnPhonePlugged(string serial)
    {
        if (_window?.IsShown != true && Settings.Prefs?.OpenOnPlug == true)
        {
            ShowWindow(Section.Overview);
        }
    }

    /// <summary>Opens (creating it the first time) or brings forward the main window.</summary>
    void ShowWindow(Section? section)
    {
        _window ??= new MainWindow(L.T("gui.title"));
        _window.ShowAt(section);
    }

    /// <summary>Quits: cancels a running test, stops the DLL, then exits (Task 5 also removes the tray icon).</summary>
    public async Task QuitAsync()
    {
        if (_lifetime is not null)
        {
            await _lifetime.QuitAsync(CancelWait);
        }
        L.Hub = null;
        Exit();
    }
}
