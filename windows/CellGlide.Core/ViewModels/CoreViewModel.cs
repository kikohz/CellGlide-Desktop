using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.ComponentModel;

namespace CellGlide.Core.ViewModels;

/// <summary>Sends commands through the hub; a failed one leaves its sentence in <see cref="Error"/>.</summary>
public abstract class CoreViewModel(EventHub hub) : ObservableObject
{
    string? _error;

    protected EventHub Hub { get; } = hub;

    /// <summary>The last failed command's message (already in the user's language), or null.</summary>
    public string? Error
    {
        get => _error;
        protected set => SetProperty(ref _error, value);
    }

    /// <summary>Sends <paramref name="command"/>: its value on success; on failure null, with <see cref="Error"/> set.</summary>
    protected async Task<ReplyValue?> RunAsync(Command command)
    {
        Error = null;
        switch (await Hub.SendAsync(command))
        {
            case ReplyOk ok:
                return ok.Value;
            case ReplyErr err:
                Error = err.Message;
                return null;
            default:
                return null;
        }
    }
}
