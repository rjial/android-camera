/* OBS source "ACAM Camera": shows the phone camera that a running acam (GUI or --headless)
 * shares over a unix socket (a named pipe on Windows), without a virtual webcam driver.
 * Wire format ("ACVF") is in PROTOCOL.md. */

#ifdef _WIN32
#define _CRT_SECURE_NO_WARNINGS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

#include <obs-module.h>
#include <util/platform.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
/* From util/threading.h, which needs OBS's w32-pthreads headers on Windows. */
EXPORT void os_set_thread_name(const char *name);
#else
#include <util/threading.h>

#include <errno.h>
#include <pthread.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
#endif

OBS_DECLARE_MODULE()
OBS_MODULE_USE_DEFAULT_LOCALE("acam-source", "en-US")

#define HEADER_LEN 24
#define MAX_DIM 8192
#define STATS_INTERVAL_NS (60ull * 1000000000ull)

/* ---- Platform layer: the connection to acam, a lock and the reader thread. ---- */
#ifdef _WIN32
typedef HANDLE conn_t; /* named pipe client end, opened for overlapped reads */
#define CONN_NONE NULL
#define PATH_LEN 256
typedef SRWLOCK lock_t;
#define lock_init(l) InitializeSRWLock(l)
#define lock_destroy(l) ((void)(l))
#define lock_take(l) AcquireSRWLockExclusive(l)
#define lock_give(l) ReleaseSRWLockExclusive(l)
typedef HANDLE thread_t;
#else
typedef int conn_t; /* unix socket */
#define CONN_NONE (-1)
#define PATH_LEN sizeof(((struct sockaddr_un *)0)->sun_path)
typedef pthread_mutex_t lock_t;
#define lock_init(l) pthread_mutex_init(l, NULL)
#define lock_destroy(l) pthread_mutex_destroy(l)
#define lock_take(l) pthread_mutex_lock(l)
#define lock_give(l) pthread_mutex_unlock(l)
typedef pthread_t thread_t;
#endif

struct acam_source {
	obs_source_t *source;
	thread_t thread;
	lock_t lock;
	char *path; /* guarded by lock */
	conn_t fd;  /* guarded by lock; CONN_NONE when not connected */
#ifdef _WIN32
	HANDLE wake;     /* manual-reset: set by kick() to abort a pending read */
	HANDLE io_event; /* for the overlapped reads */
#endif
	volatile bool stop;
	volatile bool no_signal; /* show the no-signal picture instead of clearing the source */
	uint8_t *buf;
	size_t buf_size;
	/* Last stream size (the no-signal picture uses it) and the cached picture. */
	uint32_t last_w, last_h;
	uint8_t *ns_buf;
	uint32_t ns_w, ns_h;
	int ns_reason;
	int cur_color;
	struct obs_source_frame frame;
};

/* Reasons sent by acam in the idle header, plus one of our own. */
#define REASON_ACAM_GONE 100

static const char *reason_caption(int reason)
{
	switch (reason) {
	case 0:
		return obs_module_text("NoSignal.Disconnected");
	case 1:
		return obs_module_text("NoSignal.Lost");
	case 2:
		return obs_module_text("NoSignal.SharingOff");
	case REASON_ACAM_GONE:
		return obs_module_text("NoSignal.AcamGone");
	default:
		return obs_module_text("NoSignal.Unknown");
	}
}

/* ---- No-signal picture: same design as pc/src/nosignal.rs, keep the two in step. ---- */

/* 5x7 glyphs A-Z, one byte per row, bit 4 = leftmost column. */
static const uint8_t FONT[26][7] = {
	{0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11}, {0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E},
	{0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E}, {0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E},
	{0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F}, {0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10},
	{0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F}, {0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11},
	{0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E}, {0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C},
	{0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11}, {0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F},
	{0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11}, {0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11},
	{0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E}, {0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10},
	{0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D}, {0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11},
	{0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E}, {0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04},
	{0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E}, {0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04},
	{0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A}, {0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11},
	{0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04}, {0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F},
};

/* Uppercases UTF-8 text and strips Latin-1 accents so a translation fits the A-Z font;
 * other characters become spaces. */
static void fold_text(const char *in, char *out, size_t len)
{
	static const char LATIN1[] = "AAAAAAACEEEEIIII" /* U+00C0..CF */
				     "DNOOOOO OUUUUY  " /* U+00D0..DF */;
	size_t n = 0;
	for (const unsigned char *p = (const unsigned char *)in; *p && n + 1 < len;) {
		char c = ' ';
		if (*p < 0x80) {
			c = (*p >= 'a' && *p <= 'z') ? (char)(*p - 32) : (char)*p;
			p++;
		} else if ((*p == 0xC3) && p[1] >= 0x80 && p[1] <= 0xBF) {
			/* U+00C0..U+00FF: upper and lower case share the table. */
			c = LATIN1[(p[1] - 0x80) & 0x1F];
			p += 2;
		} else {
			p++;
			while ((*p & 0xC0) == 0x80)
				p++;
		}
		out[n++] = c;
	}
	out[n] = 0;
}

struct text {
	const char *s;
	uint32_t x, y, scale;
};

static uint32_t text_width(const char *s, uint32_t scale)
{
	size_t n = strlen(s);
	return n ? (uint32_t)(n * 6 - 1) * scale : 0;
}

static bool text_hit(const struct text *t, uint32_t x, uint32_t y)
{
	uint32_t cell = 6 * t->scale;
	if (x < t->x || y < t->y || y >= t->y + 7 * t->scale || x >= t->x + text_width(t->s, t->scale))
		return false;
	uint32_t dx = x - t->x, row = (y - t->y) / t->scale, col = (dx % cell) / t->scale;
	char c = t->s[dx / cell];
	return col < 5 && c >= 'A' && c <= 'Z' && (FONT[c - 'A'][row] >> (4 - col) & 1);
}

static void bars(uint32_t x, uint32_t y, uint32_t w, uint32_t h, uint8_t rgb[3])
{
	static const uint8_t TOP[7][3] = {{191, 191, 191}, {191, 191, 0}, {0, 191, 191}, {0, 191, 0},
					  {191, 0, 191},   {191, 0, 0},   {0, 0, 191}};
	static const uint8_t MID[7][3] = {{0, 0, 191},   {19, 19, 19},  {191, 0, 191}, {19, 19, 19},
					  {0, 191, 191}, {19, 19, 19}, {191, 191, 191}};
	uint32_t bar = x * 7 / w;
	if (bar > 6)
		bar = 6;
	const uint8_t *c;
	if (y < h * 2 / 3) {
		c = TOP[bar];
	} else if (y < h * 3 / 4) {
		c = MID[bar];
	} else {
		static const uint8_t I[3] = {0, 33, 76}, W[3] = {255, 255, 255}, Q[3] = {50, 0, 106},
				     K[3] = {19, 19, 19};
		uint32_t seg = x * 28 / w;
		c = seg <= 4 ? I : seg <= 9 ? W : seg <= 14 ? Q : K;
	}
	memcpy(rgb, c, 3);
}

/* Packed I420, limited range; BT.709 from 720 lines up, else BT.601. */
static void render_nosignal(uint8_t *out, uint32_t w, uint32_t h, const char *caption_text)
{
	char title[128], caption[128];
	fold_text(obs_module_text("NoSignal.Title"), title, sizeof(title));
	fold_text(caption_text, caption, sizeof(caption));
	uint32_t ts = h / 90 ? h / 90 : 1, cs = h / 180 ? h / 180 : 1, pad = 4 * ts;
	uint32_t tw = text_width(title, ts), cw = text_width(caption, cs);
	uint32_t bw = (tw > cw ? tw : cw) + 2 * pad, bh = 7 * ts + 7 * cs + 3 * pad;
	if (bw > w)
		bw = w;
	if (bh > h)
		bh = h;
	uint32_t bx = (w - bw) / 2, by = (h - bh) / 2;
	struct text texts[2] = {
		{title, w > tw ? (w - tw) / 2 : 0, by + pad, ts},
		{caption, w > cw ? (w - cw) / 2 : 0, by + 2 * pad + 7 * ts, cs},
	};
	float kr = h >= 720 ? 0.2126f : 0.299f, kb = h >= 720 ? 0.0722f : 0.114f;
	uint8_t *yp = out, *up = out + (size_t)w * h, *vp = up + (size_t)(w / 2) * (h / 2);

	for (uint32_t y = 0; y < h; y++) {
		for (uint32_t x = 0; x < w; x++) {
			uint8_t rgb[3];
			if (x >= bx && x < bx + bw && y >= by && y < by + bh) {
				uint8_t v = text_hit(&texts[0], x, y) || text_hit(&texts[1], x, y) ? 235 : 16;
				rgb[0] = rgb[1] = rgb[2] = v;
			} else {
				bars(x, y, w, h, rgb);
			}
			float r = rgb[0] / 255.f, g = rgb[1] / 255.f, b = rgb[2] / 255.f;
			float ly = kr * r + (1 - kr - kb) * g + kb * b;
			yp[(size_t)y * w + x] = (uint8_t)(16 + 219 * ly + 0.5f);
			if (!(x & 1) && !(y & 1)) {
				size_t i = (size_t)(y / 2) * (w / 2) + x / 2;
				up[i] = (uint8_t)(128 + 224 * (b - ly) / (2 * (1 - kb)) + 0.5f);
				vp[i] = (uint8_t)(128 + 224 * (r - ly) / (2 * (1 - kr)) + 0.5f);
			}
		}
	}
}

static void output_i420(struct acam_source *s, uint8_t *data, uint32_t w, uint32_t h, int color, uint64_t ts)
{
	struct obs_source_frame *f = &s->frame;
	if (color != s->cur_color) {
		video_format_get_parameters_for_format(color == 1 ? VIDEO_CS_709 : VIDEO_CS_601, VIDEO_RANGE_PARTIAL,
						       VIDEO_FORMAT_I420, f->color_matrix, f->color_range_min,
						       f->color_range_max);
		s->cur_color = color;
	}
	f->format = VIDEO_FORMAT_I420;
	f->full_range = false;
	f->width = w;
	f->height = h;
	f->data[0] = data;
	f->data[1] = data + (size_t)w * h;
	f->data[2] = f->data[1] + (size_t)(w / 2) * (h / 2);
	f->linesize[0] = w;
	f->linesize[1] = f->linesize[2] = w / 2;
	f->timestamp = ts;
	/* OBS copies the planes, so the buffer can be reused right away. */
	obs_source_output_video(s->source, f);
}

/* Shows the no-signal picture (or clears the source when that option is off). */
static void show_nosignal(struct acam_source *s, int reason)
{
	if (!s->no_signal) {
		obs_source_output_video(s->source, NULL);
		return;
	}
	uint32_t w = s->last_w, h = s->last_h;
	if (!s->ns_buf || s->ns_w != w || s->ns_h != h || s->ns_reason != reason) {
		s->ns_buf = brealloc(s->ns_buf, (size_t)w * h * 3 / 2);
		render_nosignal(s->ns_buf, w, h, reason_caption(reason));
		s->ns_w = w;
		s->ns_h = h;
		s->ns_reason = reason;
	}
	output_i420(s, s->ns_buf, w, h, h >= 720 ? 1 : 0, os_gettime_ns());
}

static void default_socket_path(char *out, size_t len)
{
	const char *env = getenv("ACAM_SOCKET");
	if (env && *env) {
		snprintf(out, len, "%s", env);
		return;
	}
#ifdef _WIN32
	snprintf(out, len, "%s", "\\\\.\\pipe\\acam-video");
#else
	const char *xdg = getenv("XDG_RUNTIME_DIR");
	if (xdg && *xdg)
		snprintf(out, len, "%s/acam/video.sock", xdg);
	else
		snprintf(out, len, "/tmp/acam-%u/video.sock", (unsigned)getuid());
#endif
}

static uint32_t rd32(const uint8_t *p)
{
	return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static uint64_t rd64(const uint8_t *p)
{
	return (uint64_t)rd32(p) | (uint64_t)rd32(p + 4) << 32;
}

#ifdef _WIN32
/* Overlapped read, so kick() can abort it through the wake event. */
static bool read_full(struct acam_source *s, conn_t fd, uint8_t *dst, size_t len)
{
	while (len > 0) {
		OVERLAPPED ov = {0};
		ov.hEvent = s->io_event;
		DWORD n = 0, want = len > (1u << 30) ? (1u << 30) : (DWORD)len;
		if (!ReadFile(fd, dst, want, NULL, &ov)) {
			if (GetLastError() != ERROR_IO_PENDING)
				return false;
			HANDLE events[2] = {s->io_event, s->wake};
			if (WaitForMultipleObjects(2, events, FALSE, INFINITE) != WAIT_OBJECT_0) {
				CancelIoEx(fd, &ov);
				GetOverlappedResult(fd, &ov, &n, TRUE);
				return false;
			}
		}
		if (!GetOverlappedResult(fd, &ov, &n, FALSE) || n == 0)
			return false;
		dst += n;
		len -= n;
	}
	return true;
}

static conn_t connect_socket(struct acam_source *s)
{
	char path[PATH_LEN];
	lock_take(&s->lock);
	snprintf(path, sizeof(path), "%s", s->path);
	lock_give(&s->lock);

	HANDLE h = CreateFileA(path, GENERIC_READ, 0, NULL, OPEN_EXISTING, FILE_FLAG_OVERLAPPED, NULL);
	if (h == INVALID_HANDLE_VALUE)
		return CONN_NONE;

	lock_take(&s->lock);
	if (s->stop) {
		lock_give(&s->lock);
		CloseHandle(h);
		return CONN_NONE;
	}
	ResetEvent(s->wake);
	s->fd = h;
	lock_give(&s->lock);
	return h;
}

static void close_conn(conn_t fd)
{
	CloseHandle(fd);
}
#else
static bool read_full(struct acam_source *s, conn_t fd, uint8_t *dst, size_t len)
{
	UNUSED_PARAMETER(s);
	while (len > 0) {
		ssize_t n = recv(fd, dst, len, MSG_WAITALL);
		if (n < 0 && errno == EINTR)
			continue;
		if (n <= 0)
			return false;
		dst += n;
		len -= (size_t)n;
	}
	return true;
}

static conn_t connect_socket(struct acam_source *s)
{
	struct sockaddr_un addr = {.sun_family = AF_UNIX};
	lock_take(&s->lock);
	snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", s->path);
	lock_give(&s->lock);

	int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (fd < 0)
		return CONN_NONE;
	if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
		close(fd);
		return CONN_NONE;
	}
	int size = 8 << 20;
	setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &size, sizeof(size));

	lock_take(&s->lock);
	if (s->stop) {
		lock_give(&s->lock);
		close(fd);
		return CONN_NONE;
	}
	s->fd = fd;
	lock_give(&s->lock);
	return fd;
}

static void close_conn(conn_t fd)
{
	close(fd);
}
#endif

/* Reads frames until the socket closes or carries garbage. */
static void read_frames(struct acam_source *s, conn_t fd)
{
	uint8_t hdr[HEADER_LEN];
	uint64_t lat_sum = 0, lat_count = 0, lat_max = 0, stats_at = os_gettime_ns() + STATS_INTERVAL_NS;

	while (!s->stop && read_full(s, fd, hdr, HEADER_LEN)) {
		if (memcmp(hdr, "ACVF", 4) != 0) {
			blog(LOG_WARNING, "[acam] invalid header, reconnecting");
			break;
		}
		uint32_t w = rd32(hdr + 4), h = rd32(hdr + 8);
		uint64_t pts = rd64(hdr + 12);
		int color = hdr[20];

		if (w == 0 || h == 0) {
			/* No video from the phone; byte 22 says why. */
			show_nosignal(s, hdr[22]);
			continue;
		}
		if (w > MAX_DIM || h > MAX_DIM || (w & 1) || (h & 1)) {
			blog(LOG_WARNING, "[acam] invalid frame size %ux%u", w, h);
			break;
		}
		size_t size = (size_t)w * h * 3 / 2;
		if (size > s->buf_size) {
			s->buf = brealloc(s->buf, size);
			s->buf_size = size;
		}
		if (!read_full(s, fd, s->buf, size))
			break;

		uint64_t now = os_gettime_ns();
		if (now > pts) {
			uint64_t lat = now - pts;
			lat_sum += lat;
			lat_count++;
			if (lat > lat_max)
				lat_max = lat;
		}
		if (now >= stats_at && lat_count > 0) {
			blog(LOG_INFO, "[acam] %ux%u, socket latency avg %.2f ms, max %.2f ms (%llu frames)", w, h,
			     lat_sum / (double)lat_count / 1e6, lat_max / 1e6, (unsigned long long)lat_count);
			lat_sum = lat_count = lat_max = 0;
			stats_at = now + STATS_INTERVAL_NS;
		}

		s->last_w = w;
		s->last_h = h;
		/* Arrival time, not the phone's pts: show every frame as soon as it lands. */
		output_i420(s, s->buf, w, h, color, now);
	}
}

static void reader_loop(struct acam_source *s)
{
	bool warned = false;
	os_set_thread_name("acam-source");

	while (!s->stop) {
		conn_t fd = connect_socket(s);
		if (fd == CONN_NONE) {
			show_nosignal(s, REASON_ACAM_GONE);
			if (!warned) {
				lock_take(&s->lock);
				blog(LOG_INFO, "[acam] waiting for acam on %s", s->path);
				lock_give(&s->lock);
				warned = true;
			}
			for (int i = 0; i < 10 && !s->stop; i++)
				os_sleep_ms(100);
			continue;
		}
		blog(LOG_INFO, "[acam] connected to acam");
		warned = false;
		read_frames(s, fd);

		lock_take(&s->lock);
		s->fd = CONN_NONE;
		lock_give(&s->lock);
		close_conn(fd);
		if (!s->stop)
			blog(LOG_INFO, "[acam] disconnected from acam");
	}
}

#ifdef _WIN32
static DWORD WINAPI reader_thread(LPVOID data)
{
	reader_loop(data);
	return 0;
}

static bool thread_start(struct acam_source *s)
{
	s->thread = CreateThread(NULL, 0, reader_thread, s, 0, NULL);
	return s->thread != NULL;
}

static void thread_join(struct acam_source *s)
{
	WaitForSingleObject(s->thread, INFINITE);
	CloseHandle(s->thread);
}

/* Aborts the reader's pending read; it reconnects (or exits when stopping). */
static void kick(struct acam_source *s)
{
	lock_take(&s->lock);
	if (s->fd != CONN_NONE)
		SetEvent(s->wake);
	lock_give(&s->lock);
}
#else
static void *reader_thread(void *data)
{
	reader_loop(data);
	return NULL;
}

static bool thread_start(struct acam_source *s)
{
	return pthread_create(&s->thread, NULL, reader_thread, s) == 0;
}

static void thread_join(struct acam_source *s)
{
	pthread_join(s->thread, NULL);
}

/* Wakes the reader out of recv(); it reconnects (or exits when stopping). */
static void kick(struct acam_source *s)
{
	lock_take(&s->lock);
	if (s->fd >= 0)
		shutdown(s->fd, SHUT_RDWR);
	lock_give(&s->lock);
}
#endif

static const char *acam_get_name(void *unused)
{
	UNUSED_PARAMETER(unused);
	return obs_module_text("AcamCamera");
}

static void acam_update(void *data, obs_data_t *settings)
{
	struct acam_source *s = data;
	s->no_signal = obs_data_get_bool(settings, "no_signal");
	const char *path = obs_data_get_string(settings, "socket");
	char def[PATH_LEN];
	if (!path || !*path) {
		default_socket_path(def, sizeof(def));
		path = def;
	}
	lock_take(&s->lock);
	bool changed = !s->path || strcmp(s->path, path) != 0;
	if (changed) {
		bfree(s->path);
		s->path = bstrdup(path);
	}
	lock_give(&s->lock);
	if (changed)
		kick(s);
}

static void *acam_create(obs_data_t *settings, obs_source_t *source)
{
	struct acam_source *s = bzalloc(sizeof(*s));
	s->source = source;
	s->fd = CONN_NONE;
	s->last_w = 1280;
	s->last_h = 720;
	s->ns_reason = -1;
	s->cur_color = -1;
	lock_init(&s->lock);
#ifdef _WIN32
	s->wake = CreateEventA(NULL, TRUE, FALSE, NULL);
	s->io_event = CreateEventA(NULL, TRUE, FALSE, NULL);
#endif
	/* Async sources buffer frames for smooth playback by default; skip that for low latency. */
	obs_source_set_async_unbuffered(source, true);
	acam_update(s, settings);
	if (!thread_start(s)) {
		blog(LOG_ERROR, "[acam] failed to create thread");
		lock_destroy(&s->lock);
		bfree(s->path);
		bfree(s);
		return NULL;
	}
	return s;
}

static void acam_destroy(void *data)
{
	struct acam_source *s = data;
	s->stop = true;
	kick(s);
	thread_join(s);
	lock_destroy(&s->lock);
#ifdef _WIN32
	CloseHandle(s->wake);
	CloseHandle(s->io_event);
#endif
	bfree(s->path);
	bfree(s->buf);
	bfree(s->ns_buf);
	bfree(s);
}

static void acam_get_defaults(obs_data_t *settings)
{
	char def[PATH_LEN];
	default_socket_path(def, sizeof(def));
	obs_data_set_default_string(settings, "socket", def);
	obs_data_set_default_bool(settings, "no_signal", true);
}

static obs_properties_t *acam_get_properties(void *unused)
{
	UNUSED_PARAMETER(unused);
	obs_properties_t *props = obs_properties_create();
	obs_properties_add_text(props, "info", obs_module_text("Info"), OBS_TEXT_INFO);
	obs_properties_add_bool(props, "no_signal", obs_module_text("ShowNoSignal"));
	obs_properties_add_text(props, "socket", obs_module_text("Socket"), OBS_TEXT_DEFAULT);
	return props;
}

static struct obs_source_info acam_source_info = {
	.id = "acam_source",
	.type = OBS_SOURCE_TYPE_INPUT,
	.output_flags = OBS_SOURCE_ASYNC_VIDEO | OBS_SOURCE_DO_NOT_DUPLICATE,
	.get_name = acam_get_name,
	.create = acam_create,
	.destroy = acam_destroy,
	.update = acam_update,
	.get_defaults = acam_get_defaults,
	.get_properties = acam_get_properties,
	.icon_type = OBS_ICON_TYPE_CAMERA,
};

bool obs_module_load(void)
{
	obs_register_source(&acam_source_info);
	blog(LOG_INFO, "[acam] ACAM source %s loaded", ACAM_VERSION);
	return true;
}

const char *obs_module_name(void)
{
	return "ACAM Camera";
}

const char *obs_module_description(void)
{
	return obs_module_text("ModuleDescription");
}
