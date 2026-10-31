using CellGlide.Core.Dispatch;

namespace CellGlide.Core.Desktop;

/// <summary>
/// The host's one "quitting" flag and the start of the DLL that may set it: once quitting (the user quit,
/// or the DLL would not start), nothing opens a window any more.
/// </summary>
public sealed class HostGate
{
    /// <summary>The process exit code when the DLL cannot be started.</summary>
    public const int StartFailureExitCode = 2;

    volatile bool _quitting;

    /// <summary>Set once quitting has begun: windows close for real, nothing opens any more.</summary>
    public bool IsQuitting => _quitting;

    /// <summary>From now on nothing opens.</summary>
    public void Quit() => _quitting = true;

    /// <summary>Runs <paramref name="open"/> unless quitting; whether it ran.</summary>
    public bool Open(Action open)
    {
        if (_quitting)
        {
            return false;
        }
        open();
        return true;
    }

    /// <summary>
    /// Starts the DLL through <paramref name="start"/> (<see cref="CoreStart.TryStart"/>). When it cannot start,
    /// quitting is set before <paramref name="showFailure"/> runs: that message is modal, its message loop
    /// delivers a later start of the app redirected here meanwhile, and that start must open nothing (there is
    /// no shell to show); <paramref name="exitCode"/> is then <see cref="StartFailureExitCode"/>, else 0.
    /// </summary>
    public ICore? Start(Func<ICore> start, Action showFailure, out int exitCode)
    {
        if (CoreStart.TryStart(start) is { } core)
        {
            exitCode = 0;
            return core;
        }
        Quit();
        showFailure();
        exitCode = StartFailureExitCode;
        return null;
    }
}
