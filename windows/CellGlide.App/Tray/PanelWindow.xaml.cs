using System.Runtime.InteropServices;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using Windows.System;

namespace CellGlide.App.Tray;

/// <summary>
/// The panel the tray icon opens: no title bar, fixed size, above other windows, not in the taskbar
/// or Alt+Tab, placed next to the icon (<see cref="PanelPlacement"/>). It hides when it loses
/// activation, on Esc and on another tray click; a tray click right after it hid on losing
/// activation is the click that took the activation away, so it does not reopen it (cg-gui's
/// REOPEN_GUARD).
/// </summary>
public sealed partial class PanelWindow : Window
{
    const int WidthDips = 340;
    const int HeightDips = 460;
    const int MarginDips = 12;

    readonly Guid _trayId;
    readonly Action<Section?> _openWindow;
    readonly Func<Task> _quit;
    readonly ReopenGuard _reopenGuard = new();
    bool _isOpen;

    /// <param name="trayId">The tray icon's GUID, to ask the shell where it is.</param>
    /// <param name="openWindow">Opens the main window (at a section, or where it was).</param>
    /// <param name="quit">Quits the app.</param>
    public PanelWindow(ShellViewModel shell, Guid trayId, Action<Section?> openWindow, Func<Task> quit)
    {
        Shell = shell;
        _trayId = trayId;
        _openWindow = openWindow;
        _quit = quit;
        InitializeComponent();
        Title = L.T("gui.title");

        var presenter = OverlappedPresenter.Create();
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        presenter.IsAlwaysOnTop = true;
        presenter.SetBorderAndTitleBar(hasBorder: true, hasTitleBar: false);
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        SystemBackdrop = new DesktopAcrylicBackdrop();

        Activated += OnActivated;
        AppWindow.Closing += OnClosing;
        // The dots' brushes follow a switch to or from high contrast.
        Show.Changed += () => Bindings.Update();
    }

    public ShellViewModel Shell { get; }

    /// <summary>Whether the panel is on screen.</summary>
    public bool IsOpen => _isOpen;

    /// <summary>Where the panel was last put, in physical pixels (for checks and logs).</summary>
    public RectInt32 LastPlacement { get; private set; }

    /// <summary>A tray click: opens a hidden panel, hides an open one.</summary>
    public void Toggle()
    {
        if (_isOpen)
        {
            Dismiss();
            return;
        }
        if (_reopenGuard.AllowsOpen(Now()))
        {
            Open();
        }
    }

    /// <summary>Hides the panel (Esc, a second tray click, a choice made in it, quitting).</summary>
    public void Dismiss()
    {
        if (!_isOpen)
        {
            return;
        }
        _isOpen = false;
        AppWindow.Hide();
    }

    void Open()
    {
        var icon = TrayNative.IconRect(_trayId);
        var (work, dpi) = TrayNative.WorkArea(icon);
        var width = TrayNative.Scale(WidthDips, dpi);
        var height = TrayNative.Scale(HeightDips, dpi);
        var at = PanelPlacement.Place(icon, work, TrayNative.TaskbarEdge(), width, height, TrayNative.Scale(MarginDips, dpi));
        var rect = new RectInt32(at.X, at.Y, width, height);
        AppWindow.MoveAndResize(rect);
        _isOpen = true;
        AppWindow.Show(activateWindow: true);
        Activate();
        _ = SetForegroundWindow(Win32Interop.GetWindowFromWindowId(AppWindow.Id));
        // Moving onto a monitor with another scale may have resized it on the way.
        AppWindow.MoveAndResize(rect);
        LastPlacement = rect;
    }

    void OnActivated(object sender, WindowActivatedEventArgs args)
    {
        if (args.WindowActivationState == WindowActivationState.Deactivated && _isOpen)
        {
            Dismiss();
            _reopenGuard.ClosedOnDeactivation(Now());
        }
    }

    static TimeSpan Now() => TimeSpan.FromMilliseconds(Environment.TickCount64);

    void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        // Alt+F4 hides it like any other way of closing; only quitting closes it.
        if (!App.Current.IsQuitting)
        {
            args.Cancel = true;
            Dismiss();
        }
    }

    void OnRootKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Escape)
        {
            e.Handled = true;
            Dismiss();
        }
    }

    void OnHealthClick(object sender, RoutedEventArgs e)
    {
        Dismiss();
        _openWindow(Section.History);
    }

    void OnOpenClick(object sender, RoutedEventArgs e)
    {
        Dismiss();
        _openWindow(null);
    }

    void OnQuitClick(object sender, RoutedEventArgs e)
    {
        Dismiss();
        _ = _quit();
    }

    // ---- For x:Bind: which of the DLL's texts and tones to show, never new ones. ----

    Brush ErrorBrush => Show.Brush(Tone.Red);

    /// <summary>The state's tone; grey while there is no state (cg_ipc status_tone without a snapshot).</summary>
    Brush StatusBrush(OverviewView? overview) => Show.Brush(overview?.StatusTone ?? Tone.Gray);

    /// <summary>The state's title, else why there is none, else that the service is being reached.</summary>
    string TitleOf(OverviewView? overview, ServiceGoneEvent? down) =>
        overview?.Title ?? down?.Text ?? L.T("gui.waiting_service");

    string DetailOf(OverviewView? overview) => overview?.Detail ?? "";

    /// <summary>The state's button label; without a state, the menu's connect entry.</summary>
    string ButtonLabel(OverviewView? overview, ChromeView? chrome) =>
        overview?.ButtonLabel
        ?? chrome?.Menu.FirstOrDefault(item => item.Kind == MenuKind.Toggle)?.Label
        ?? "";

    /// <summary>Prominent for connecting; plain for disconnecting, which is not the thing to invite.</summary>
    Style ButtonStyle(OverviewView? overview) =>
        (Style)Application.Current.Resources[overview?.WantsConnection == true ? "DefaultButtonStyle" : "AccentButtonStyle"];

    PhoneCardView? PhoneOf(OverviewView? overview) => overview?.Phone;

    Visibility HealthVisible(OverviewView? overview) => Show.Visible(overview?.Health);

    Brush HealthBrush(OverviewView? overview) => Show.Brush(overview?.Health?.Tone ?? Tone.Gray);

    string HealthLine(OverviewView? overview) => overview?.Health?.Line ?? "";

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetForegroundWindow(nint hwnd);
}
