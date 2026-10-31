using CellGlide.Core.Dispatch;
using CellGlide.Core.Models;
using CommunityToolkit.Mvvm.Input;

namespace CellGlide.Core.ViewModels;

/// <summary>The About page: the version, the Telegram channel and the licenses, as the DLL gives them.</summary>
public sealed class AboutViewModel : CoreViewModel
{
    AboutValue? _about;

    public AboutViewModel(EventHub hub)
        : base(hub)
    {
        LoadCommand = new AsyncRelayCommand(LoadAsync);
    }

    /// <summary>Null until <see cref="LoadCommand"/> has succeeded.</summary>
    public AboutValue? About
    {
        get => _about;
        private set => SetProperty(ref _about, value);
    }

    public IAsyncRelayCommand LoadCommand { get; }

    async Task LoadAsync()
    {
        if (await RunAsync(new GetAboutCommand()) is AboutValue about)
        {
            About = about;
        }
    }
}
