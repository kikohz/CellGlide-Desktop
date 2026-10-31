using System.Security.Cryptography;
using System.Text;

namespace CellGlide.Core.Desktop;

/// <summary>
/// The tray icon's GUID. Windows ties a notification icon's GUID to the path of the exe that first
/// registered it, so the GUID is made from that path: the same install always gets the same icon
/// (and keeps the user's "show in taskbar" choice), another copy elsewhere gets its own.
/// </summary>
public static class TrayIdentity
{
    public static readonly Guid Namespace = new("6f1d3a52-8c2e-4b7a-9e43-2d5c0b8f71a4");

    /// <summary>
    /// RFC 4122 version 5 (name-based, SHA-1) in <see cref="Namespace"/>, the name being the UTF-8 of
    /// the full path in upper case; equal to Python's <c>uuid.uuid5(Namespace, name)</c>.
    /// </summary>
    public static Guid GuidFor(string exePath)
    {
        var name = Encoding.UTF8.GetBytes(Path.GetFullPath(exePath).ToUpperInvariant());
        // The namespace in network byte order, then the name.
        var input = new byte[16 + name.Length];
        Namespace.ToByteArray(bigEndian: true).CopyTo(input, 0);
        name.CopyTo(input, 16);
        var b = SHA1.HashData(input)[..16];
        b[6] = (byte)((b[6] & 0x0F) | 0x50);
        b[8] = (byte)((b[8] & 0x3F) | 0x80);
        return new Guid(b, bigEndian: true);
    }
}
