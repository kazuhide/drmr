#!/bin/sh
set -eu

# Tests select immutable fixture behavior through the invoking symlink's name.
mode=${0##*/}
case "$mode" in
    exit-error) exit 1 ;;
    invalid-json) printf '%s' 'not json' ;;
    empty-streams) printf '%s' '{"streams": []}' ;;
    he-aac)
        printf '%s' '{"streams": [{"codec_type": "audio", "codec_name": "aac", "profile": "HE-AAC"}]}'
        ;;
    ffprobe|missing-keyframe|packet-error)
        case "$*" in
            *-show_streams*)
                if [ "$mode" = ffprobe ]; then
                    printf '%s' '{"streams": [{"index": 0, "codec_type": "video", "codec_name": "h264"}]}'
                else
                    printf '%s' '{"streams": [{"codec_type": "video", "codec_name": "h264"}]}'
                fi
                ;;
            *-show_packets*)
                case "$mode" in
                    ffprobe) printf '%s' '{"packets": [{"flags": "K_"}]}' ;;
                    missing-keyframe) printf '%s' '{"packets": [{"flags": "__"}]}' ;;
                    packet-error) exit 1 ;;
                esac
                ;;
            *) exit 2 ;;
        esac
        ;;
    *) exit 2 ;;
esac
