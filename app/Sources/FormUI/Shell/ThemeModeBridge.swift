import FormCore
import FormDesign

/// `FormCore.ThemeMode` is an open string value — unknown modes round-trip rather than
/// failing to decode (spec 00 §6) — while `FormDesign.ThemeMode` is the closed enum
/// `ThemeController` resolves. The shell is the only place the two meet.
public extension FormDesign.ThemeMode {
    init(_ core: FormCore.ThemeMode) {
        switch core {
        case .light: self = .light
        case .dark: self = .dark
        default: self = .system
        }
    }

    var core: FormCore.ThemeMode {
        switch self {
        case .light: .light
        case .dark: .dark
        case .system: .system
        }
    }
}
