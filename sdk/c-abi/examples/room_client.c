#include "fluvora.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void usage(void) {
    fputs(
        "usage: fluvora-c-demo <command> [arguments]\n"
        "  create <sfu|p2p|live|vod>\n"
        "  join <room-id>\n"
        "  chat <room-id> <text>\n"
        "  custom <room-id> <payload-json>\n"
        "  ice <room-id>\n"
        "  sfu-offer <room-id> <offer-sdp>\n"
        "  p2p-signal <room-id> <recipient-id> <kind> <payload-json>\n"
        "Environment: FLUVORA_BASE_URL and required FLUVORA_ACCESS_TOKEN\n",
        stderr);
}

static int print_result(int status, char *json) {
    if (status == FLUVORA_OK && json != NULL) {
        puts(json);
    } else {
        fprintf(stderr, "Fluvora operation failed with status %d\n", status);
    }
    fluvora_string_free(json);
    return status == FLUVORA_OK ? EXIT_SUCCESS : EXIT_FAILURE;
}

int main(int argc, char **argv) {
    const char *base_url = getenv("FLUVORA_BASE_URL");
    const char *token = getenv("FLUVORA_ACCESS_TOKEN");
    char *json = NULL;
    int status = FLUVORA_INVALID_ARGUMENT;
    int result = EXIT_FAILURE;
    int attempted = 0;

    if (argc < 2 || token == NULL || token[0] == '\0') {
        usage();
        return EXIT_FAILURE;
    }
    if (base_url == NULL || base_url[0] == '\0') {
        base_url = "http://127.0.0.1:8080";
    }

    FluvoraClient *client = fluvora_client_new(base_url, token);
    if (client == NULL) {
        fputs("failed to create Fluvora client\n", stderr);
        return EXIT_FAILURE;
    }

    if (strcmp(argv[1], "create") == 0 && argc == 3) {
        attempted = 1;
        status = fluvora_create_room(client, argv[2], 64, 16, &json);
    } else if (strcmp(argv[1], "join") == 0 && argc == 3) {
        attempted = 1;
        status = fluvora_join_room(client, argv[2], &json);
    } else if (strcmp(argv[1], "chat") == 0 && argc == 4) {
        attempted = 1;
        status = fluvora_send_chat(client, argv[2], "c-demo-message", argv[3], &json);
    } else if (strcmp(argv[1], "custom") == 0 && argc == 4) {
        attempted = 1;
        status = fluvora_send_custom_data(client, argv[2], "demo.c", 1, argv[3], &json);
    } else if (strcmp(argv[1], "ice") == 0 && argc == 3) {
        attempted = 1;
        status = fluvora_get_ice_configuration(client, argv[2], &json);
    } else if (strcmp(argv[1], "sfu-offer") == 0 && argc == 4) {
        /*
         * argv[3] is a complete ICE-gathered SDP string from the host PeerConnection.
         * Apply answer_sdp from the JSON result back to that same PeerConnection.
         */
        attempted = 1;
        status = fluvora_exchange_offer(client, argv[2], argv[3], &json);
    } else if (strcmp(argv[1], "p2p-signal") == 0 && argc == 6) {
        attempted = 1;
        status = fluvora_post_signal(client, argv[2], argv[3], argv[4], argv[5], &json);
    } else {
        usage();
    }
    if (attempted != 0) {
        result = print_result(status, json);
    }

    fluvora_client_free(client);
    return result;
}
