#ifndef FLUVORA_H
#define FLUVORA_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32) && !defined(FLUVORA_STATIC)
#define FLUVORA_API __declspec(dllimport)
#else
#define FLUVORA_API
#endif

typedef struct FluvoraClient FluvoraClient;

/* Input byte limits exclude the required trailing NUL. */
#define FLUVORA_MAX_BASE_URL_BYTES 2048u
#define FLUVORA_MAX_ACCESS_TOKEN_BYTES 4096u
#define FLUVORA_MAX_IDENTIFIER_BYTES 32u
#define FLUVORA_MAX_NAME_BYTES 128u
#define FLUVORA_MAX_STRUCTURED_INPUT_BYTES (1024u * 1024u)

enum FluvoraStatus {
    FLUVORA_OK = 0,
    FLUVORA_INVALID_ARGUMENT = 1,
    FLUVORA_SDK_ERROR = 2,
    FLUVORA_ENCODING_ERROR = 3,
    FLUVORA_PANIC = 4
};

FLUVORA_API FluvoraClient *fluvora_client_new(
    const char *base_url,
    const char *access_token);

FLUVORA_API void fluvora_client_free(FluvoraClient *client);
FLUVORA_API void fluvora_string_free(char *value);

FLUVORA_API int fluvora_client_set_access_token(
    FluvoraClient *client,
    const char *access_token);

FLUVORA_API int fluvora_create_room(
    FluvoraClient *client,
    const char *mode,
    size_t max_members,
    size_t max_publishers,
    char **out_json);

FLUVORA_API int fluvora_get_room(
    FluvoraClient *client,
    const char *room_id,
    char **out_json);

/* operation accepts "join", "leave", "end", "publish_start", or "publish_stop". */
FLUVORA_API int fluvora_room_command(
    FluvoraClient *client,
    const char *room_id,
    const char *operation,
    char **out_json);

FLUVORA_API int fluvora_join_room(
    FluvoraClient *client,
    const char *room_id,
    char **out_json);

FLUVORA_API int fluvora_leave_room(
    FluvoraClient *client,
    const char *room_id,
    char **out_json);

FLUVORA_API int fluvora_send_chat(
    FluvoraClient *client,
    const char *room_id,
    const char *message_id,
    const char *text,
    char **out_json);

FLUVORA_API int fluvora_send_custom_data(
    FluvoraClient *client,
    const char *room_id,
    const char *namespace_name,
    uint16_t schema_version,
    const char *payload_json,
    char **out_json);

FLUVORA_API int fluvora_exchange_offer(
    FluvoraClient *client,
    const char *room_id,
    const char *offer_sdp,
    char **out_json);

FLUVORA_API int fluvora_post_signal(
    FluvoraClient *client,
    const char *room_id,
    const char *recipient_id,
    const char *kind,
    const char *payload_json,
    char **out_json);

FLUVORA_API int fluvora_poll_signals(
    FluvoraClient *client,
    const char *room_id,
    uint64_t after_sequence,
    char **out_json);

FLUVORA_API int fluvora_get_ice_configuration(
    FluvoraClient *client,
    const char *room_id,
    char **out_json);

#ifdef __cplusplus
}
#endif

#endif
