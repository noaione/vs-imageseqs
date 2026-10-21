# libheif enables optional codecs when it finds their development files. Keep
# the native dependency set independent of the image the build runs in,
# whether that is one of the wheel containers or a plain runner.
foreach(package X265 X264 kvazaar UVG266 vvdec vvenc OpenH264 AOM SvtEnc
                RAV1E JPEG OpenJPEG OPENJPH FFMPEG Brotli)
    set(CMAKE_DISABLE_FIND_PACKAGE_${package} TRUE CACHE BOOL "" FORCE)
endforeach()

# Libheif's default enables libsharpyuv colour transforms, which this plugin
# does not use. Force it off so a build cannot pick up a library from the
# runner and add it to the wheel.
set(WITH_LIBSHARPYUV OFF CACHE BOOL "" FORCE)
