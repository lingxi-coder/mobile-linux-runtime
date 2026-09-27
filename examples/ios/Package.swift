// swift-tools-version: 5.9
import PackageDescription
let package=Package(name:"MobileLinuxSample",platforms:[.iOS("18.0")],products:[.library(name:"MobileLinuxSample",targets:["MobileLinuxSample"])],dependencies:[.package(path:"../../ios")],targets:[.target(name:"MobileLinuxSample",dependencies:[.product(name:"MobileLinuxRuntime",package:"ios")],path:"Sources")])
