using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace CellGlide.App;

/// <summary>The entry point (the XAML-generated Main is disabled; Task 4 adds single-instance handling here).</summary>
public static class Program
{
    [STAThread]
    static void Main()
    {
        WinRT.ComWrappersSupport.InitializeComWrappers();
        Application.Start(static _ =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            new App();
        });
    }
}
