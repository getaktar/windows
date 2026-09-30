// Makes the photos src/image_metadata.rs is tested with: a small image
// with an iPhone-like location, camera, lens and date, in each format.
//
//   swift tests/fixtures/make_fixtures.swift   (from src-tauri, on a Mac)
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let folder = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
let context = CGContext(
    data: nil, width: 64, height: 48, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
)!
context.setFillColor(CGColor(srgbRed: 0.07, green: 0.37, blue: 1, alpha: 1))
context.fill(CGRect(x: 0, y: 0, width: 64, height: 48))
let image = context.makeImage()!

let properties: [CFString: Any] = [
    kCGImagePropertyOrientation: 6,
    kCGImagePropertyGPSDictionary: [
        kCGImagePropertyGPSLatitude: 41.0082,
        kCGImagePropertyGPSLatitudeRef: "N",
        kCGImagePropertyGPSLongitude: 28.9784,
        kCGImagePropertyGPSLongitudeRef: "E",
        kCGImagePropertyGPSAltitude: 39.0,
    ],
    kCGImagePropertyTIFFDictionary: [
        kCGImagePropertyTIFFMake: "Apple",
        kCGImagePropertyTIFFModel: "iPhone 17 Pro",
        kCGImagePropertyTIFFDateTime: "2026:09:30 12:00:00",
        kCGImagePropertyTIFFOrientation: 6,
    ],
    kCGImagePropertyExifDictionary: [
        kCGImagePropertyExifDateTimeOriginal: "2026:09:30 12:00:00",
        kCGImagePropertyExifLensModel: "iPhone 17 Pro back camera",
    ],
    kCGImageDestinationLossyCompressionQuality: 0.8,
]

for (name, type) in [("location.jpg", UTType.jpeg), ("location.heic", UTType.heic), ("location.png", UTType.png), ("location.tiff", UTType.tiff)] {
    let url = folder.appendingPathComponent(name)
    let destination = CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, properties as CFDictionary)
    guard CGImageDestinationFinalize(destination) else { fatalError("Couldn't write \(name)") }
    print("Wrote \(url.path)")
}
