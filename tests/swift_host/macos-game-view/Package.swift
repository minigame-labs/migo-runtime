// swift-tools-version: 5.9
//
// A macOS app, as small as one can be, that runs a game through the product
// surface: `MigoMacV8.MigoGameView`, with nothing from the C ABI in sight.
// Driven by scripts/test-macos-game-view.sh, which signs it the way the V8
// lane ships (hardened runtime + allow-jit) and reads what it prints.
import PackageDescription

let package = Package(
    name: "MigoGameViewHost",
    platforms: [.macOS(.v11)],
    dependencies: [.package(name: "Migo", path: "../../../platforms/apple")],
    targets: [
        .executableTarget(
            name: "MigoGameViewHost",
            dependencies: [.product(name: "MigoMacV8", package: "Migo")],
            path: "Sources/MigoGameViewHost"
        )
    ]
)
