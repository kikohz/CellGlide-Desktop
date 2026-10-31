using System.Text.Json;
using System.Text.Json.Serialization;

namespace CellGlide.Core.Models;

/// <summary>
/// The source-generated JSON for every message between the app and cellglide_native.dll:
/// snake_case fields, every null written (as Rust writes them), and the type tags read wherever
/// they appear in an object.
/// </summary>
[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    DefaultIgnoreCondition = JsonIgnoreCondition.Never,
    AllowOutOfOrderMetadataProperties = true)]
[JsonSerializable(typeof(Command))]
[JsonSerializable(typeof(CoreEvent))]
[JsonSerializable(typeof(Dictionary<string, string>))]
public sealed partial class CoreJsonContext : JsonSerializerContext;

/// <summary>A data-free Rust enum: its variants as snake_case strings.</summary>
public sealed class SnakeCaseEnumConverter<TEnum>() : JsonStringEnumConverter<TEnum>(JsonNamingPolicy.SnakeCaseLower, allowIntegerValues: false)
    where TEnum : struct, Enum;
