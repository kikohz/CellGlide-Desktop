using CellGlide.Core.Models;

namespace CellGlide.Core.Desktop;

/// <summary>
/// How the app was started: <c>--tray</c> (logging in, from the Run key) starts it without a window;
/// <c>CELLGLIDE_SPEED_CAPS=small</c> (development and acceptance only) makes every speed test a small one.
/// </summary>
public sealed record LaunchOptions(bool Tray, SpeedCaps? SpeedCaps)
{
    /// <summary>The limits <c>CELLGLIDE_SPEED_CAPS=small</c> stands for.</summary>
    public static readonly SpeedCaps Small = new(10_000_000, 5_000_000, 5);

    /// <summary>Reads <c>--tray</c> (any case) from <paramref name="args"/>, ignoring everything else, and the small caps from the variable.</summary>
    public static LaunchOptions Parse(IReadOnlyList<string> args, string? speedCapsEnv)
    {
        var tray = args.Any(a => string.Equals(a, "--tray", StringComparison.OrdinalIgnoreCase));
        var caps = string.Equals(speedCapsEnv, "small", StringComparison.OrdinalIgnoreCase) ? Small : null;
        return new LaunchOptions(tray, caps);
    }
}
