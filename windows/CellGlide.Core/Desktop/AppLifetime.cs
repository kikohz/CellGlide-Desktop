using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CellGlide.Core.ViewModels;

namespace CellGlide.Core.Desktop;

/// <summary>
/// The quit order: cancel a running speed or leak test, wait a little for the DLL to confirm, then
/// close the hub and stop the DLL (after which nothing calls back).
/// </summary>
public sealed class AppLifetime(EventHub hub, ICore core, SpeedViewModel speed, LeakViewModel leak)
{
    Task? _quit;

    /// <summary>
    /// Quits, waiting at most <paramref name="cancelWait"/> for the cancels' replies (none may come).
    /// Every call returns the first call's task.
    /// </summary>
    public Task QuitAsync(TimeSpan cancelWait)
    {
        var mine = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        if (Interlocked.CompareExchange(ref _quit, mine.Task, null) is { } earlier)
        {
            return earlier;
        }
        _ = RunAsync(cancelWait, mine);
        return mine.Task;
    }

    async Task RunAsync(TimeSpan cancelWait, TaskCompletionSource done)
    {
        try
        {
            var replies = new List<Task>(2);
            if (speed.View?.Running == true)
            {
                replies.Add(hub.SendAsync(new SpeedCancelCommand()));
            }
            if (leak.View?.Running == true)
            {
                replies.Add(hub.SendAsync(new LeakCancelCommand()));
            }
            try
            {
                await Task.WhenAll(replies).WaitAsync(cancelWait);
            }
            catch (Exception)
            {
                // No reply in time, or the hub could not deliver it: quit anyway.
            }
            hub.Dispose();
            core.Dispose();
            done.SetResult();
        }
        catch (Exception e)
        {
            done.SetException(e);
        }
    }
}
