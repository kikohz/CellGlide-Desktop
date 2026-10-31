using System.ComponentModel;
using System.Diagnostics;
using CellGlide.App.Hosting;
using CellGlide.Core.Desktop;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;
using CommunityToolkit.Mvvm.Input;
using H.NotifyIcon;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Controls;
using Windows.UI.ViewManagement;

namespace CellGlide.App.Tray;

/// <summary>
/// The notification-area icon. Its look, tooltip and menu are the DLL's chrome
/// (<see cref="ShellViewModel.Chrome"/>); only the choice of icon file for a light or dark taskbar is
/// made here. Left click toggles the panel; right click shows a native menu built from
/// <see cref="ChromeView.Menu"/>, in its order, with its labels and enabled states.
/// </summary>
sealed class TrayHost : IDisposable
{
    readonly ShellViewModel _shell;
    readonly DispatcherQueue _queue;
    readonly Action _togglePanel;
    readonly Action _open;
    readonly Func<Task> _quit;
    readonly TaskbarIcon _icon;
    readonly MenuFlyout _menu = new();
    readonly UISettings _settings = new();
    string? _iconFile;
    int _iconSize;
    IReadOnlyList<MenuItemView>? _menuShown;
    bool _disposed;

    /// <param name="id">The icon's GUID (<see cref="TrayIdentity.GuidFor"/> of this exe).</param>
    /// <param name="togglePanel">Opens or closes the panel.</param>
    /// <param name="open">Opens the main window where it was.</param>
    /// <param name="quit">Quits the app.</param>
    public TrayHost(Guid id, ShellViewModel shell, DispatcherQueue queue, Action togglePanel, Action open, Func<Task> quit)
    {
        _shell = shell;
        _queue = queue;
        _togglePanel = togglePanel;
        _open = open;
        _quit = quit;
        Id = id;
        _icon = new TaskbarIcon
        {
            Id = id,
            ContextMenuMode = ContextMenuMode.PopupMenu,
            // A click goes straight to the panel (no wait for a double click), so a click that
            // closed the panel by taking its focus is still within the panel's reopen guard.
            NoLeftClickDelay = true,
            // Raised on a timer thread: the panel is the UI thread's.
            LeftClickCommand = new RelayCommand(() => Post(_togglePanel)),
            ContextFlyout = _menu,
        };
        Update();
        try
        {
            _icon.ForceCreate(enablesEfficiencyMode: false);
        }
        catch (Exception e)
        {
            // Another CellGlide with the same exe (a second process after a late redirect) or a
            // shell that is not up yet: no icon for now, but no crash. The library recreates the
            // icon when the taskbar (re)starts.
            Debug.WriteLine("CellGlide: the tray icon could not be added: " + e.Message);
        }
        shell.PropertyChanged += OnShellChanged;
        // Raised on a background thread when the Windows mode (light or dark) changes.
        _settings.ColorValuesChanged += OnColorValuesChanged;
        // The icon's frame follows the scale of the monitor it is on: its message window gets
        // WM_DPICHANGED when that scale changes, and a restarted taskbar may sit elsewhere.
        _icon.TrayIcon.MessageWindow.DpiChanged += OnPlacementChanged;
        _icon.TrayIcon.MessageWindow.TaskbarCreated += OnPlacementChanged;
    }

    /// <summary>The icon's GUID; the panel asks the shell where this icon is.</summary>
    public Guid Id { get; }

    /// <summary>Whether the shell has the icon.</summary>
    public bool IsCreated => !_disposed && _icon.IsCreated;

    /// <summary>Removes the icon.</summary>
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }
        _disposed = true;
        _shell.PropertyChanged -= OnShellChanged;
        _settings.ColorValuesChanged -= OnColorValuesChanged;
        _icon.TrayIcon.MessageWindow.DpiChanged -= OnPlacementChanged;
        _icon.TrayIcon.MessageWindow.TaskbarCreated -= OnPlacementChanged;
        _icon.Dispose();
    }

    void OnShellChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ShellViewModel.Chrome))
        {
            Update();
        }
    }

    void OnColorValuesChanged(UISettings sender, object args) => Post(Update);

    void OnPlacementChanged(object? sender, EventArgs e) => Post(Update);

    /// <summary>
    /// Shows the current chrome: icon file, tooltip, menu. The taskbar's mode and the icon's size are
    /// read again every time (also on each state event), so they are never stale for long even if a
    /// change notification is missed; the menu is rebuilt only when its items differ.
    /// </summary>
    void Update()
    {
        if (_disposed)
        {
            return;
        }
        var chrome = _shell.Chrome;
        // Before the first state or service-gone event: the plain icon and the product name.
        var look = chrome?.Tray ?? new TrayLook(false, false, false, L.T("gui.title"));
        try
        {
            UpdateIcon(look);
            if (_icon.ToolTipText != look.Title)
            {
                _icon.ToolTipText = look.Title;
            }
        }
        catch (Exception e)
        {
            // The shell lost the icon (Explorer restarting); it is added again with these values.
            Debug.WriteLine("CellGlide: updating the tray icon failed: " + e.Message);
        }
        if (chrome is not null && !TrayMenu.Same(_menuShown, chrome.Menu))
        {
            BuildMenu(chrome.Menu);
        }
    }

    void UpdateIcon(TrayLook look)
    {
        var file = TrayIcons.FileFor(look, TrayNative.LightTaskbar());
        var size = TrayNative.TrayIconSize(TrayNative.IconRect(Id));
        if (file == _iconFile && size == _iconSize)
        {
            return;
        }
        // The frame closest to the tray's size at this DPI; the old icon is disposed by the setter.
        _icon.Icon = new System.Drawing.Icon(Path.Combine(AppContext.BaseDirectory, file), size, size);
        _iconFile = file;
        _iconSize = size;
    }

    void BuildMenu(IReadOnlyList<MenuItemView> items)
    {
        _menu.Items.Clear();
        foreach (var item in items)
        {
            if (item.Kind == MenuKind.Separator)
            {
                _menu.Items.Add(new MenuFlyoutSeparator());
                continue;
            }
            _menu.Items.Add(new MenuFlyoutItem
            {
                Text = item.Label,
                // The version line is only shown, never clicked.
                IsEnabled = item.Enabled && item.Kind != MenuKind.Version,
                Command = CommandFor(item.Kind),
            });
        }
        _menuShown = items;
    }

    RelayCommand? CommandFor(MenuKind kind) => kind switch
    {
        // Run after the menu's own message loop has returned.
        MenuKind.Toggle => new RelayCommand(() => Post(ToggleConnection)),
        MenuKind.Open => new RelayCommand(() => Post(_open)),
        MenuKind.Quit => new RelayCommand(() => Post(() => _ = _quit())),
        _ => null,
    };

    /// <summary>The menu's connect or disconnect: not again while the last one is being carried out.</summary>
    void ToggleConnection()
    {
        if (_shell.ToggleCommand.CanExecute(null))
        {
            _shell.ToggleCommand.Execute(null);
        }
    }

    void Post(Action action)
    {
        if (!_queue.TryEnqueue(() => action()))
        {
            Debug.WriteLine("CellGlide: the UI thread is gone; a tray action was dropped");
        }
    }
}
