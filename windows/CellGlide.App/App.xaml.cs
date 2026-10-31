using CellGlide.App.Shell;
using CellGlide.Core.Native;
using Microsoft.UI.Xaml;

namespace CellGlide.App;

/// <summary>
/// Skeleton (Task 4 replaces it): start the DLL in the system's language, open the main window
/// titled by the DLL, and quit (disposing the DLL) when the window closes.
/// </summary>
public partial class App : Application
{
    NativeCore? _core;
    MainWindow? _window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _core = NativeCore.Start("");
        _window = new MainWindow(_core.Tr("gui.title"));
        _window.Closed += (_, _) =>
        {
            _core?.Dispose();
            _core = null;
            Exit();
        };
        _window.Activate();
    }
}
