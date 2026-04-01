/* Video source that calls into Rust for MJPEG frame data, or generates YUYV bars */

#include <errno.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <linux/videodev2.h>

#include "events.h"
#include "timer.h"
#include "tools.h"
#include "video-buffers.h"
#include "video-source.h"

/* Implemented in Rust */
extern unsigned int rust_fill_jpeg(void *buf, unsigned int max_size);
extern int rust_camera_start(void);
extern void rust_camera_stop(void);

struct rust_source {
	struct video_source src;
	struct timer *timer;
	unsigned int width;
	unsigned int height;
	unsigned int pixelformat;
	int streaming;
};

#define to_rust_source(s) container_of(s, struct rust_source, src)

static void rust_source_destroy(struct video_source *s)
{
	struct rust_source *src = to_rust_source(s);
	timer_destroy(src->timer);
	free(src);
}

static int rust_source_set_format(struct video_source *s,
				  struct v4l2_pix_format *fmt)
{
	struct rust_source *src = to_rust_source(s);
	src->width = fmt->width;
	src->height = fmt->height;
	src->pixelformat = fmt->pixelformat;
	printf("rust-source: format set to %.4s %ux%u\n",
	       (char *)&fmt->pixelformat, fmt->width, fmt->height);
	return 0;
}

static int rust_source_set_frame_rate(struct video_source *s, unsigned int fps)
{
	struct rust_source *src = to_rust_source(s);
	if (fps == 0)
		fps = 30;
	printf("rust-source: fps=%u\n", fps);
	timer_set_fps(src->timer, fps);
	return 0;
}

static int rust_source_free_buffers(struct video_source *s __attribute__((unused)))
{
	return 0;
}

static int rust_source_stream_on(struct video_source *s)
{
	struct rust_source *src = to_rust_source(s);
	int ret = rust_camera_start();
	if (ret)
		return ret;
	ret = timer_arm(src->timer);
	if (ret) {
		rust_camera_stop();
		return ret;
	}
	src->streaming = 1;
	return 0;
}

static int rust_source_stream_off(struct video_source *s)
{
	struct rust_source *src = to_rust_source(s);
	int ret = timer_disarm(src->timer);
	src->streaming = 0;
	rust_camera_stop();
	return ret;
}

/* YUYV color bar colors */
#define WHITE   0x80eb80eb
#define YELLOW  0x8adb10db
#define CYAN    0x10bc9abc
#define GREEN   0x2aad1aad
#define MAGENTA 0xe64ed64e
#define RED     0xf03f663f
#define BLUE    0x7620f020
#define BLACK   0x80108010

static void fill_yuyv_bars(struct rust_source *src, struct video_buffer *buf)
{
	unsigned int bpl = src->width * 2;
	unsigned int i, j;
	void *mem = buf->mem;

	for (i = 0; i < src->height; ++i) {
		for (j = 0; j < bpl; j += 4) {
			unsigned int val;
			if      (j < bpl * 1 / 8) val = WHITE;
			else if (j < bpl * 2 / 8) val = YELLOW;
			else if (j < bpl * 3 / 8) val = CYAN;
			else if (j < bpl * 4 / 8) val = GREEN;
			else if (j < bpl * 5 / 8) val = MAGENTA;
			else if (j < bpl * 6 / 8) val = RED;
			else if (j < bpl * 7 / 8) val = BLUE;
			else                       val = BLACK;
			*(unsigned int *)(mem + i * bpl + j) = val;
		}
	}
	buf->bytesused = bpl * src->height;
}

static void rust_source_fill_buffer(struct video_source *s,
				    struct video_buffer *buf)
{
	struct rust_source *src = to_rust_source(s);

	if (src->pixelformat == v4l2_fourcc('M', 'J', 'P', 'G')) {
		buf->bytesused = rust_fill_jpeg(buf->mem, buf->size);
	} else {
		fill_yuyv_bars(src, buf);
	}

	if (src->streaming)
		timer_wait(src->timer);
}

static const struct video_source_ops rust_source_ops = {
	.destroy = rust_source_destroy,
	.set_format = rust_source_set_format,
	.set_frame_rate = rust_source_set_frame_rate,
	.free_buffers = rust_source_free_buffers,
	.stream_on = rust_source_stream_on,
	.stream_off = rust_source_stream_off,
	.queue_buffer = NULL,
	.fill_buffer = rust_source_fill_buffer,
};

struct video_source *rust_video_source_create(void)
{
	struct rust_source *src = malloc(sizeof *src);
	if (!src)
		return NULL;

	memset(src, 0, sizeof *src);
	src->src.ops = &rust_source_ops;
	src->src.type = VIDEO_SOURCE_STATIC;
	src->timer = timer_new();
	if (!src->timer) {
		free(src);
		return NULL;
	}

	return &src->src;
}
