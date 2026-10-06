# libheif-sys enables every codec backend while building its embedded copy.
# This plugin needs libde265 for HEIC and dav1d for animated AVIF tracks.
# Still AVIF items also use dav1d directly. Disable unused backends so the Mac
# binary cannot pick up optional codecs installed on the build runner.
set(WITH_LIBDE265 ON CACHE BOOL "" FORCE)

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

# libheif-sys asks libheif for its libsharpyuv colour transforms too, and the
# runner image has homebrew's webp installed, so libheif found it. Nothing
# here encodes, and leaving it on put libsharpyuv.0.1.2.dylib in the delocated
# bundle, where no symbol referred to it and `package-macos-wheel.py` refused
# it by name.
set(WITH_LIBSHARPYUV OFF CACHE BOOL "" FORCE)
