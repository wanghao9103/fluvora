// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "Fluvora",
    platforms: [
        .iOS(.v16),
        .macOS(.v13),
    ],
    products: [
        .library(name: "Fluvora", targets: ["Fluvora"]),
        .executable(name: "fluvora-swift-demo", targets: ["FluvoraDemo"]),
    ],
    targets: [
        .target(name: "Fluvora"),
        .executableTarget(name: "FluvoraDemo", dependencies: ["Fluvora"]),
        .testTarget(name: "FluvoraTests", dependencies: ["Fluvora"]),
    ]
)
