using Microsoft.UI.Xaml;

namespace CellGlide.App.Shell;

/// <summary>The main window (skeleton; Task 6 fills it).</summary>
public sealed partial class MainWindow : Window
{
    public MainWindow(string title)
    {
        InitializeComponent();
        Title = title;
    }
}
