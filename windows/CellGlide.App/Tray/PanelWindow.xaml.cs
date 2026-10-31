using System.Runtime.InteropServices;
using CellGlide.App.Hosting;
using CellGlide.App.Shell;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using CommunityToolkit.Mvvm.Input;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
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
    FocusState? _focusOnOpen;

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
        Root.Loaded += (_, _) => FocusFirst();
        AppWindow.Closing += OnClosing;
        // The dots' brushes follow a switch to or from high contrast.
        Show.Changed += () => Bindings.Update();
        // The main button is off while the toggle runs (from here, the window or the tray menu).
        shell.ToggleCommand.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(IAsyncRelayCommand.IsRunning))
            {
                ApplyBusy();
            }
        };
        // A new state (or why there is none) and a failed command are read out.
        Show.AnnounceChanges(TitleLine);
        Show.AnnounceChanges(ErrorLine);
    }

    public ShellViewModel Shell { get; }

    /// <summary>Whether the panel is on screen.</summary>
    public bool IsOpen => _isOpen;

    /// <summary>Where the panel was last put, in physical pixels (for checks and logs).</summary>
    public RectInt32 LastPlacement { get; private set; }

    /// <summary>
    /// A tray click or key: opens a hidden panel, hides an open one. The opened panel gives the focus to its
    /// first control, shown as keyboard focus when <paramref name="fromKeyboard"/>.
    /// </summary>
    public void Toggle(bool fromKeyboard = false)
    {
        if (_isOpen)
        {
            Dismiss();
            return;
        }
        if (_reopenGuard.AllowsOpen(Now()))
        {
            Open(fromKeyboard ? FocusState.Keyboard : FocusState.Programmatic);
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

    void Open(FocusState focus)
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
        _focusOnOpen = focus;
        FocusFirst();
    }

    /// <summary>
    /// Gives the focus to the connect button, or the first control that is on (while a toggle runs, Open
    /// CellGlide). The first time the panel opens its content is not loaded yet; it is tried again when the
    /// content has loaded and when the window is activated, until it takes.
    /// </summary>
    void FocusFirst()
    {
        if (_isOpen && _focusOnOpen is { } focus
            && FocusManager.FindFirstFocusableElement(Root) is Control first && first.Focus(focus))
        {
            _focusOnOpen = null;
        }
    }

    void OnActivated(object sender, WindowActivatedEventArgs args)
    {
        if (args.WindowActivationState == WindowActivationState.Deactivated && _isOpen)
        {
            Dismiss();
            _reopenGuard.ClosedOnDeactivation(Now());
        }
        else if (args.WindowActivationState != WindowActivationState.Deactivated)
        {
            FocusFirst();
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

    /// <summary>
    /// Off while the toggle runs, through Show.SetEnabled: keyboard focus goes to Open CellGlide and comes back
    /// when it is done (a Command binding would turn the button off with the focus still on it).
    /// </summary>
    void ApplyBusy() => Show.SetEnabled(MainButton, !Shell.ToggleCommand.IsRunning, OpenButton);

    async void OnMainClick(object sender, RoutedEventArgs e)
    {
        // A second press while it runs is refused here as the command would refuse it (cg-gui's command_pending).
        if (Shell.ToggleCommand.CanExecute(null))
        {
            // A failed toggle leaves its sentence in Error, shown above.
            await Shell.ToggleCommand.ExecuteAsync(null);
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

    // ---- For x:Bind (the rest is in Show, bound to Shell.Now): the DLL's texts and tones, never new ones. ----

    Brush ErrorBrush => Show.Brush(Tone.Red);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetForegroundWindow(nint hwnd);
}
