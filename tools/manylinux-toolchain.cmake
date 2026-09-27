# libheif-sys enables optional codecs when it finds their development files.
# Keep the release's native dependency set independent of the container image.
foreach(package X265 X264 kvazaar UVG266 vvdec vvenc OpenH264 AOM SvtEnc
                RAV1E JPEG OpenJPEG OPENJPH FFMPEG)
    set(CMAKE_DISABLE_FIND_PACKAGE_${package} TRUE CACHE BOOL "" FORCE)
endforeach()
