namespace CellGlide.Core.Dispatch;

/// <summary>Runs work on the UI thread, later; never waits for it.</summary>
public interface IUiDispatcher
{
    void Post(Action action);
}

/// <summary>Posts to a SynchronizationContext (the UI thread's).</summary>
public sealed class SynchronizationContextDispatcher(SynchronizationContext context) : IUiDispatcher
{
    public void Post(Action action) => context.Post(static state => ((Action)state!)(), action);
}

/// <summary>Runs at once on the calling thread: for tests and console tools.</summary>
public sealed class InlineDispatcher : IUiDispatcher
{
    public void Post(Action action) => action();
}
