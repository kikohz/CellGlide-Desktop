using System.Runtime.InteropServices;
using CellGlide.Core.Models;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace CellGlide.App.Shell;

/// <summary>The main window (Task 6 fills it). Closing it hides it; the app keeps running in the tray.</summary>
public sealed partial class MainWindow : Window
{
    public MainWindow(string title)
    {
        InitializeComponent();
        Title = title;
        AppWindow.Closing += OnClosing;
    }

    /// <summary>Whether the window is on screen (shown, possibly minimized), not hidden.</summary>
    public bool IsShown => AppWindow.IsVisible;

    /// <summary>
    /// Shows the window (restored if minimized) and brings it to the front; with a section, opens that
    /// page (Task 6), without one keeps the page it was on.
    /// </summary>
    public void ShowAt(Section? section)
    {
        _ = section;    // Task 6 navigates here.
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
        {
            presenter.Restore();
        }
        AppWindow.Show();
        Activate();
        _ = SetForegroundWindow(Win32Interop.GetWindowFromWindowId(AppWindow.Id));
    }

    static void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        // Decision 9: closing the window only hides it; quitting is the tray's (and About's) Quit.
        args.Cancel = true;
        sender.Hide();
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetForegroundWindow(nint hwnd);
}
