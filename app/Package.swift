// swift-tools-version: 6.0
import PackageDescription

// Target boundaries are ownership boundaries — see docs/specs/15-build-and-conventions.md.
// This file is frozen after W0: if a workstream needs a target or dependency change, it asks
// rather than editing, because every parallel agent builds against it.
//
// The Rust static library is linked by the Makefile, which passes
//   -Xlinker -L<core/target/<profile>>
// The `link "form_ffi"` directive in Sources/FormFFI/module.modulemap supplies the -l.

let package = Package(
    name: "form",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "form", targets: ["form"]),
        .library(name: "FormCore", targets: ["FormCore"]),
        .library(name: "FormDesign", targets: ["FormDesign"]),
        .library(name: "FormMarkdown", targets: ["FormMarkdown"]),
        // Exposed as a product so the Xcode app target can link it; the Xcode target
        // compiles Sources/form and links these, keeping one copy of every source file.
        .library(name: "FormUI", targets: ["FormUI"]),
    ],
    targets: [
        // C module map over core/include/form.h. Owned by W7.
        .systemLibrary(name: "FormFFI", path: "Sources/FormFFI"),

        // The only module that touches C. Transport, protocol Codables, actor, stores. W7.
        .target(name: "FormCore", dependencies: ["FormFFI"]),

        // Theme tokens, typography, primitives. No other module defines a color or font. W8.
        .target(name: "FormDesign"),

        // Renders the block tree the core produces. Parses nothing. W11.
        .target(name: "FormMarkdown", dependencies: ["FormCore", "FormDesign"]),

        // Views. Subdirectories are per-workstream: Shell/ Sidebar/ (W9), Chat/ (W10),
        // Home/ (W12), Preferences/ Attachments/ (W13), Commands/ (W14).
        .target(name: "FormUI", dependencies: ["FormCore", "FormDesign", "FormMarkdown"]),

        // @main, window, menus. W9.
        .executableTarget(name: "form", dependencies: ["FormUI"]),

        .testTarget(name: "FormCoreTests", dependencies: ["FormCore"]),
        .testTarget(name: "FormDesignTests", dependencies: ["FormDesign"]),
        .testTarget(name: "FormMarkdownTests", dependencies: ["FormMarkdown"]),
        .testTarget(name: "FormUITests", dependencies: ["FormUI"]),
    ]
)
