using CellGlide.Core.Models;

namespace CellGlide.Core.Dispatch;

/// <summary>The DLL as the app sees it: commands in, events out, texts on demand.</summary>
public interface ICore : IDisposable
{
    /// <summary>Sends one command; returns its number (which its reply carries), 0 when not running.</summary>
    ulong Send(Command command);

    /// <summary>One text in the current language.</summary>
    string Tr(string key, IReadOnlyDictionary<string, string>? args = null);

    /// <summary>Every event, on the DLL's own thread. Never block it waiting for a thread that is starting or stopping the DLL.</summary>
    event Action<CoreEvent>? EventReceived;
}
