using System.Diagnostics;
using System.Runtime.InteropServices;
using CellGlide.Core.Desktop;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace CellGlide.App;

/// <summary>The entry point (the XAML-generated Main is disabled): launch options, then single instance.</summary>
public static partial class Program
{
    /// <summary>The single-instance key every CellGlide process registers or finds.</summary>
    const string InstanceKey = "CellGlide.Main";

    /// <summary>How long a second instance waits for the running one to take its activation.</summary>
    static readonly TimeSpan RedirectWait = TimeSpan.FromSeconds(5);

    /// <summary>
    /// Later starts of the app handed to this one. Subscribed before the app starts, so a redirect
    /// that arrives before OnLaunched is kept until the app attaches to it.
    /// </summary>
    internal static ActivationLatch Activations { get; } = new();

    /// <summary>The process's exit code; set before <see cref="Application.Exit"/> to report a failure.</summary>
    internal static int ExitCode { get; set; }

    [STAThread]
    static int Main(string[] args)
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        var options = LaunchOptions.Parse(args, Environment.GetEnvironmentVariable("CELLGLIDE_SPEED_CAPS"));
        var main = AppInstance.FindOrRegisterForKey(InstanceKey);
        switch (LaunchDecision.Decide(main.IsCurrent, options.Tray))
        {
            case Launch.Exit:
                return 0;
            case Launch.Redirect:
                if (Redirect(main))
                {
                    return 0;
                }
                // The running app did not take it: better a second app than nothing on screen (as on Linux).
                break;
        }
        AppInstance.GetCurrent().Activated += (_, _) => Activations.Signal();
        Application.Start(_ =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            new App(options);
        });
        return ExitCode;
    }

    /// <summary>Hands this launch to the running app (on a background thread, at most <see cref="RedirectWait"/>); false if that failed.</summary>
    static bool Redirect(AppInstance main)
    {
        try
        {
            // This process was started by the user, so it may let the running one come to the front.
            _ = AllowSetForegroundWindow(main.ProcessId);
            var activation = AppInstance.GetCurrent().GetActivatedEventArgs();
            var redirect = Task.Run(() => main.RedirectActivationToAsync(activation).AsTask());
            return redirect.Wait(RedirectWait);
        }
        catch (Exception e)
        {
            Debug.WriteLine("CellGlide: redirecting to the running instance failed: " + e);
            return false;
        }
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool AllowSetForegroundWindow(uint processId);
}
