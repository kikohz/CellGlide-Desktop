using System.Runtime.Versioning;
using Microsoft.Win32;

namespace CellGlide.Core.Desktop;

/// <summary>
/// The Run key under HKCU. Only our own value (<see cref="Autostart.ValueName"/>) is ever written or
/// deleted; tests pass a scratch key under HKCU as <paramref name="subKey"/>.
/// </summary>
[SupportedOSPlatform("windows")]
public sealed class RegistryRunKey(string subKey = RegistryRunKey.RunPath) : IRunKey
{
    public const string RunPath = @"Software\Microsoft\Windows\CurrentVersion\Run";

    public string? Read(string name)
    {
        using var key = Registry.CurrentUser.OpenSubKey(subKey);
        return key?.GetValue(name) as string;
    }

    public void Write(string name, string value)
    {
        Ours(name);
        using var key = Registry.CurrentUser.CreateSubKey(subKey, writable: true);
        key.SetValue(name, value, RegistryValueKind.String);
    }

    public void Delete(string name)
    {
        Ours(name);
        using var key = Registry.CurrentUser.OpenSubKey(subKey, writable: true);
        key?.DeleteValue(name, throwOnMissingValue: false);
    }

    static void Ours(string name)
    {
        if (name != Autostart.ValueName)
        {
            throw new ArgumentException("only the " + Autostart.ValueName + " value is ours", nameof(name));
        }
    }
}

/// <summary>
/// StartupApproved\Run under HKCU, read only: this class has no way to write. Windows writes it when
/// the user turns an entry off or on in Startup apps; we never override that. Tests pass a scratch key
/// as <paramref name="subKey"/> and never open the real one.
/// </summary>
[SupportedOSPlatform("windows")]
public sealed class RegistryStartupApproved(string subKey = RegistryStartupApproved.ApprovedPath) : IStartupApproved
{
    public const string ApprovedPath = @"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    public byte[]? Read(string name)
    {
        using var key = Registry.CurrentUser.OpenSubKey(subKey, writable: false);
        return key?.GetValue(name) as byte[];
    }
}
