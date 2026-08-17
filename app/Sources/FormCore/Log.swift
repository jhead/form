import Foundation
import os

/// The one logging surface in the app. Conventions forbid `print` (spec 15 §3), and the
/// unified log is what makes a streaming problem diagnosable after the fact.
///
/// Nothing here may log a secret: `KeychainStore` logs operations and status codes, never
/// values (spec 07 §5).
public enum Log {
    private static let subsystem = "dev.jhead.form"

    public static let core = Logger(subsystem: subsystem, category: "core")
    public static let events = Logger(subsystem: subsystem, category: "events")
    public static let stores = Logger(subsystem: subsystem, category: "stores")
    public static let keychain = Logger(subsystem: subsystem, category: "keychain")
    public static let ui = Logger(subsystem: subsystem, category: "ui")
}
