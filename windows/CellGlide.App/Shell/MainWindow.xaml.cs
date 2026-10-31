using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using CellGlide.App.Hosting;
using CellGlide.App.Pages;
using CellGlide.App.Tray;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;

namespace CellGlide.App.Shell;

/// <summary>
/// The main window: 880 x 640 DIP (at least 760 x 520), Mica, the DLL's window title in its own
/// title bar, and a NavigationView built from <see cref="ChromeView.Sidebar"/> with the dots of
/// <see cref="ChromeView.Badges"/> and the version (the tray menu's first entry) under it. Closing it
/// hides it; the app keeps running in the tray.
/// </summary>
public sealed partial class MainWindow : Window
{
    const int WidthDips = 880;
    const int HeightDips = 640;
    const int MinWidthDips = 760;
    const int MinHeightDips = 520;

    readonly ShellViewModel _shell;
    readonly Dictionary<Section, Page> _pages = [];
    readonly Dictionary<Section, NavigationViewItem> _items = [];
    readonly Dictionary<Section, Badge> _badges = [];
    IReadOnlyList<SidebarGroupView>? _sidebar;
    Section _section = Section.Overview;

    public MainWindow(ShellViewModel shell)
    {
        _shell = shell;
        InitializeComponent();

        ExtendsContentIntoTitleBar = true;
        SetTitleBar(AppTitleBar);
        AppWindow.TitleBar.PreferredHeightOption = TitleBarHeightOption.Tall;
        SystemBackdrop = new MicaBackdrop();
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));

        var hwnd = Win32Interop.GetWindowFromWindowId(AppWindow.Id);
        var dpi = GetDpiForWindow(hwnd);
        AppWindow.Resize(new SizeInt32(TrayNative.Scale(WidthDips, dpi), TrayNative.Scale(HeightDips, dpi)));
        KeepMinimumSize(hwnd);

        AppWindow.Closing += OnClosing;
        AppWindow.Changed += (_, e) =>
        {
            // Hidden to the tray, shown again, minimised or restored (minimising changes the size).
            if (e.DidVisibilityChange || e.DidPresenterChange || e.DidSizeChange)
            {
                ScreenChanged?.Invoke();
            }
        };
        shell.PropertyChanged += OnShellChanged;
        // The dots' brushes follow a switch to or from high contrast.
        Show.Changed += () => ApplyBadges(repaint: true);

        ApplyChrome();
        Open(Section.Overview);
    }

    /// <summary>Whether the window is on screen (shown, possibly minimized), not hidden.</summary>
    public bool IsShown => AppWindow.IsVisible;

    /// <summary>The window is minimised.</summary>
    public bool IsMinimised => AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized };

    /// <summary>Raised when the window may have been hidden, shown, minimised or restored.</summary>
    public event Action? ScreenChanged;

    /// <summary>
    /// Shows the window (restored if minimized) and brings it to the front; with a section, opens that
    /// page, without one keeps the page it was on.
    /// </summary>
    public void ShowAt(Section? section)
    {
        if (section is { } open)
        {
            Open(open);
        }
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
        {
            presenter.Restore();
        }
        AppWindow.Show();
        Activate();
        _ = SetForegroundWindow(Win32Interop.GetWindowFromWindowId(AppWindow.Id));
    }

    /// <summary>Shows a section's page (made the first time) and selects its sidebar item.</summary>
    void Open(Section section)
    {
        _section = section;
        if (!_pages.TryGetValue(section, out var page))
        {
            page = _pages[section] = Create(section);
        }
        if (!ReferenceEquals(Nav.Content, page))
        {
            Nav.Content = page;
        }
        if (_items.TryGetValue(section, out var item) && !ReferenceEquals(Nav.SelectedItem, item))
        {
            Nav.SelectedItem = item;
        }
    }

    Page Create(Section section) => section switch
    {
        Section.Overview => new OverviewPage(_shell, Open),
        Section.SpeedTest => new SpeedPage(App.Current.Speed, App.Current.Settings),
        Section.DnsLeak => new LeakPage(App.Current.Leak),
        Section.History => new HistoryPage(App.Current.History, this),
        Section.Diagnostics => new DiagnosticsPage(App.Current.Diagnostics),
        Section.Settings => new SettingsPage(App.Current.Settings, _shell, App.Current.Autostart, this),
        _ => new AboutPage(App.Current.About),
    };

    void OnSelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        // Clearing the items to rebuild them selects nothing; that is not a choice.
        if (args.SelectedItemContainer?.Tag is Section section && section != _section)
        {
            Open(section);
        }
    }

    void OnShellChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ShellViewModel.Chrome))
        {
            ApplyChrome();
        }
    }

    /// <summary>The title, sidebar, dots and version from the chrome; before any, the product name.</summary>
    void ApplyChrome()
    {
        var chrome = _shell.Chrome;
        var title = chrome?.WindowTitle ?? L.T("gui.title");
        if (TitleText.Text != title)
        {
            Title = title;
            AppWindow.Title = title;
            TitleText.Text = title;
        }
        if (chrome is null)
        {
            return;
        }
        VersionText.Text = chrome.Menu.FirstOrDefault(item => item.Kind == MenuKind.Version)?.Label ?? "";
        if (!SidebarLayout.Same(_sidebar, chrome.Sidebar))
        {
            BuildSidebar(chrome.Sidebar);
        }
        ApplyBadges(repaint: false);
    }

    /// <summary>The DLL's groups in its order: a header for each heading, an item for each section.</summary>
    void BuildSidebar(IReadOnlyList<SidebarGroupView> sidebar)
    {
        Nav.MenuItems.Clear();
        _items.Clear();
        _badges.Clear();
        foreach (var group in sidebar)
        {
            if (group.Heading is { } heading)
            {
                Nav.MenuItems.Add(new NavigationViewItemHeader { Content = heading });
            }
            foreach (var entry in group.Sections)
            {
                var item = new NavigationViewItem { Content = entry.Title, Tag = entry.Section };
                _items[entry.Section] = item;
                Nav.MenuItems.Add(item);
            }
        }
        _sidebar = sidebar;
        if (_items.TryGetValue(_section, out var selected))
        {
            Nav.SelectedItem = selected;
        }
    }

    /// <summary>
    /// An orange dot for a warning, a red one for a critical, nothing otherwise; only items whose dot
    /// changed are touched (or all, when the brushes changed).
    /// </summary>
    void ApplyBadges(bool repaint)
    {
        if (_shell.Chrome is not { } chrome)
        {
            return;
        }
        foreach (var (section, item) in _items)
        {
            var badge = SidebarBadges.For(chrome.Badges, section);
            if (!repaint && _badges.TryGetValue(section, out var shown) && shown == badge)
            {
                continue;
            }
            _badges[section] = badge;
            item.InfoBadge = badge switch
            {
                Badge.Warning => Dot(Tone.Orange),
                Badge.Critical => Dot(Tone.Red),
                _ => null,
            };
        }
    }

    static InfoBadge Dot(Tone tone)
    {
        // A dot, like cg-gui's 7 px one; the page itself says what is wrong.
        var dot = new InfoBadge { Background = Show.Brush(tone), Width = 8, Height = 8 };
        AutomationProperties.SetAccessibilityView(dot, AccessibilityView.Raw);
        return dot;
    }

    static void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        // Decision 9: closing the window only hides it; quitting is the tray's (and About's) Quit.
        if (App.Current.IsQuitting)
        {
            return;
        }
        args.Cancel = true;
        sender.Hide();
    }

    // ---- The minimum size, in DIP at the window's DPI now (WM_GETMINMAXINFO). ----
    // Not OverlappedPresenter.PreferredMinimumWidth/Height: in WinAppSDK 2.6 they are physical
    // pixels (760 gave a 760 px window at 125%), so they would not follow the window's DPI.

    const uint WM_GETMINMAXINFO = 0x0024;
    const uint WM_NCDESTROY = 0x0082;
    const int GWLP_WNDPROC = -4;

    /// <summary>Each subclassed window's previous procedure (window messages all run on the UI thread).</summary>
    static readonly Dictionary<nint, nint> s_previous = [];

    static unsafe void KeepMinimumSize(nint hwnd)
    {
        if (s_previous.ContainsKey(hwnd))
        {
            return;
        }
        delegate* unmanaged[Stdcall]<nint, uint, nint, nint, nint> proc = &WindowProc;
        s_previous[hwnd] = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, (nint)proc);
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvStdcall)])]
    static unsafe nint WindowProc(nint hwnd, uint msg, nint wParam, nint lParam)
    {
        if (!s_previous.TryGetValue(hwnd, out var previous))
        {
            return DefWindowProcW(hwnd, msg, wParam, lParam);
        }
        if (msg == WM_NCDESTROY)
        {
            // The window is going: put its own procedure back before it handles its last message.
            _ = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous);
            s_previous.Remove(hwnd);
            return CallWindowProcW(previous, hwnd, msg, wParam, lParam);
        }
        var result = CallWindowProcW(previous, hwnd, msg, wParam, lParam);
        if (msg == WM_GETMINMAXINFO && lParam != 0)
        {
            var dpi = GetDpiForWindow(hwnd);
            var info = (MINMAXINFO*)lParam;
            info->ptMinTrackSize.X = TrayNative.Scale(MinWidthDips, dpi);
            info->ptMinTrackSize.Y = TrayNative.Scale(MinHeightDips, dpi);
            return 0;
        }
        return result;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct POINT
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct MINMAXINFO
    {
        public POINT ptReserved;
        public POINT ptMaxSize;
        public POINT ptMaxPosition;
        public POINT ptMinTrackSize;
        public POINT ptMaxTrackSize;
    }

    [LibraryImport("user32.dll")]
    private static partial uint GetDpiForWindow(nint hwnd);

    [LibraryImport("user32.dll")]
    private static partial nint SetWindowLongPtrW(nint hwnd, int index, nint value);

    [LibraryImport("user32.dll")]
    private static partial nint CallWindowProcW(nint previous, nint hwnd, uint msg, nint wParam, nint lParam);

    [LibraryImport("user32.dll")]
    private static partial nint DefWindowProcW(nint hwnd, uint msg, nint wParam, nint lParam);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetForegroundWindow(nint hwnd);
}
