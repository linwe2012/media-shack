import { func } from "prop-types";
import * as ffmpeg from 'fluent-ffmpeg';
const ffmpeg_installer = require ('@ffmpeg-installer/ffmpeg');
const ffprobe_installer = require('@ffprobe-installer/ffprobe');

ffmpeg.setFfmpegPath(ffmpeg_installer.path);
ffmpeg.setFfprobePath(ffprobe_installer.path);

export interface FFTag {
    creation_time: string;
    encoder: string;
    title: string;
}

export interface FFormat {
    bit_rate: number;
    duration: number;
    filename: string; // path to the file
    format_long_name: string;
    format_name: string;
    nb_programs: number;
    nb_streams: number;
    probe_score: number;
    size: number; // file size
    start_time: number;
    tags: FFTag;

}

export interface FFStream {
    avg_frame_rate?: string;
    bit_rate?: string;
    bits_per_raw_sample?: number;
    chroma_location?: string;
    codec_long_name?: string;
    codec_name: string;
    codec_tag: string;
    codec_tag_string: string;
    codec_time_base: string;
    codec_type:  "video" | "audio";
    coded_height: number;
    coded_width: number;
    color_primaries: string;
    color_range: string;
    width ?: number;
    height ?: number;
}

export interface FFMedia {
    chapters: any;
    format?: FFormat;
    streams?: FFStream[];
}


export class Chrono {
    year : number = 0;
    month : number = 0;
    day : number = 0;
    hour : number = 0;
    minute : number = 0;
    second : number = 0;
    
    fromSecond(sec : number) {
        let min = Math.floor(sec / 60);
        this.second = sec - min * 60;
        this.fromMinute(min);
    }

    fromMinute(min : number) {
        let h = Math.floor(min / 60);
        this.minute = min - min * 60;
        this.fromHour(h);
    }

    fromHour(h: number) {
        let d = Math.floor(h / 24);
        this.hour = h - h * 24;
        this.fromDay(d);
    }
    fromDay(d: number) {
        this.day = d;
    }
}

const SniffForMedia = async (filename : string) : Promise<Media> => 
{
    let promise = new Promise<Media>((resolve, reject)=>{
        ffmpeg.ffprobe(filename, (err : any, metadata : ffmpeg.FfprobeData)=>{
            if(!err) {
                let res = new Media;
                res.meta = metadata;
                resolve(res)
            }
            else {
                reject(err);
            }
        })
    });
    return promise;
}

export class CaredMediaInfo {
    video_codec : string = '';
    audio_codec : string = '';
    width : number = -1;
    height : number = -1;
    filesize : number = -1;
    duration : Chrono = new Chrono;
}

export class Media {
    meta ?: FFMedia
    
    getCaredInfo() : CaredMediaInfo {
        let i = new CaredMediaInfo;
        if(this.meta && this.meta.format) {
            i.duration.fromSecond(
                this.meta.format.duration || 0
            );
        }

        if(this.meta && this.meta.streams) {
            for(let stream of this.meta.streams) {
                if(stream.codec_type === 'video') {
                    i.video_codec = stream.codec_name;
                    i.width = stream.width || -1;
                    i.height = stream.height || -1;
                }
                else if(stream.codec_type === 'audio') {
                    i.audio_codec = stream.codec_name;
                }
            }
        }
        
        return i;
    }
}