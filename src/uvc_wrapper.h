// Wrapper for bindgen — include the uvc-gadget public headers
#include "config.h"
#include "configfs.h"
#include "events.h"
#include "stream.h"
#include "test-source.h"
#include "video-source.h"

// Our custom source
struct video_source *rust_video_source_create(void);
