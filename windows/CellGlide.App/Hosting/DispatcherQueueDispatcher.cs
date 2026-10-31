using CellGlide.Core.Dispatch;
using Microsoft.UI.Dispatching;

namespace CellGlide.App.Hosting;

/// <summary>Posts to the UI thread's DispatcherQueue.</summary>
public sealed class DispatcherQueueDispatcher(DispatcherQueue queue) : IUiDispatcher
{
    /// <summary>
    /// Enqueues <paramref name="action"/>; throws when the queue has shut down, so that a command
    /// waiting for its reply ends with that exception instead of never.
    /// </summary>
    public void Post(Action action)
    {
        if (!queue.TryEnqueue(new DispatcherQueueHandler(action)))
        {
            throw new InvalidOperationException("the UI thread's DispatcherQueue is shut down");
        }
    }
}
