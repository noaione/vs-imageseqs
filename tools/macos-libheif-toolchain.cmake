# Build the pinned system libheif used by the macOS wheel. The plugin needs
# libde265 for HEIC and dav1d for animated AVIF tracks. Still AVIF items also
# use dav1d directly. Disable unused backends so the library cannot pick up
# optional codecs installed on the build runner.
set(WITH_LIBDE265 ON CACHE BOOL "" FORCE)
set(WITH_DAV1D ON CACHE BOOL "" FORCE)
set(WITH_DAV1D_PLUGIN OFF CACHE BOOL "" FORCE)
set(WITH_LIBDE265_PLUGIN OFF CACHE BOOL "" FORCE)

foreach(codec IN ITEMS
    AOM_DECODER
    AOM_ENCODER
    RAV1E
    SvtEnc
    X264
    X265
    JPEG_DECODER
    JPEG_ENCODER
    KVAZAAR
    OPENJPH_ENCODER
    OPEN_JPH_ENCODER
    OpenH264_DECODER
    OpenJPEG_DECODER
    OpenJPEG_ENCODER
    UVG266
    VVDEC
    VVENC
)
    set(WITH_${codec} OFF CACHE BOOL "" FORCE)
endforeach()

set(CMAKE_DISABLE_FIND_PACKAGE_Brotli TRUE CACHE BOOL "" FORCE)

# The runner image has homebrew's webp installed. Nothing here encodes, so do
# not link libsharpyuv into the bundle.
set(WITH_LIBSHARPYUV OFF CACHE BOOL "" FORCE)
