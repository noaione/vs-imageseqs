# libheif-sys enables optional codecs when it finds their development files.
# Keep the native dependency set independent of the image the build runs in,
# whether that is one of the wheel containers or a plain runner.
foreach(package X265 X264 kvazaar UVG266 vvdec vvenc OpenH264 AOM SvtEnc
                RAV1E JPEG OpenJPEG OPENJPH FFMPEG)
    set(CMAKE_DISABLE_FIND_PACKAGE_${package} TRUE CACHE BOOL "" FORCE)
endforeach()

# libheif-sys asks libheif for its libsharpyuv colour transforms, and libheif
# links whatever it finds. Nothing here encodes, so the dependency is dropped
# at the source rather than satisfied: the `set` overrides the
# `-DWITH_LIBSHARPYUV=ON` libheif-sys passes because a toolchain file is read
# after the command line. Without it a build only links while the image has
# sharpyuv's development files, and the plugin picks up a library none of its
# symbols come from.
set(WITH_LIBSHARPYUV OFF CACHE BOOL "" FORCE)
