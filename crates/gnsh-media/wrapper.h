#include <errno.h>
#include <libavcodec/avcodec.h>
#include <libavutil/avutil.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
#include <libavutil/pixfmt.h>
#include <SDL3/SDL.h>

static const int GNSH_AVERROR_EAGAIN = AVERROR(EAGAIN);
static const int GNSH_AVERROR_EOF = AVERROR_EOF;
