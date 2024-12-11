import { WHIPClient } from "./whip.js"
window.startCam = async () => {
    const supportedCodecs = RTCRtpSender.getCapabilities("video").codecs;
	console.log(supportedCodecs);
    if (window.whip_instance) {
        window.whip_instance.stop();
    }

    if (window.stream_instance) {
        window.stream_instance.getTracks().forEach(track => track.stop());
    }
    let constraints = {
        audio: true,
        video: {
            width: { ideal: 1920},
            height: { ideal: 1080 },
            frameRate: { ideal: 60 }
        }
    };
    //Get mic+cam
    // const stream = await navigator.mediaDevices.getDisplayMedia({ video: constraints});
    const stream = await navigator.mediaDevices.getUserMedia({audio:true, video:true});

    document.getElementById("video").srcObject = stream;

    //Create peerconnection
    const pc = new RTCPeerConnection();

    //Send all tracks
    for (const track of stream.getTracks()) {
        //You could add simulcast too here
        var tr = pc.addTransceiver(track, {
            direction: "sendonly",
            streams:  [stream],
            sendEncodings: [
                { rid: "0", active: true, maxBitrate: 8000000 },
                // { rid: "1", active: true, maxBitrate: 2000000 },
                // { rid: "2", active: true },
            ],
        });
        // console.log(tr.receiver.getParameters());
    }

    //Create whip client
    const whip = new WHIPClient();

    const url = "/whip/endpoint";
    const token = document.getElementById("room-id").value;

    //Start publishing
    whip.publish(pc, url, token);

    window.whip_instance = whip;
    window.stream_instance = stream;
}

window.startShareScreen = async () => {
    if (window.whip_instance) {
        window.whip_instance.stop();
    }

    if (window.stream_instance) {
        window.stream_instance.getTracks().forEach(track => track.stop());
    }
    let constraints = {
        audio: true,
        video: {
            width: { ideal: 640},
            height: { ideal: 480 },
            frameRate: { ideal: 30 }
        }
    };
    //Get mic+cam
    const stream = await navigator.mediaDevices.getDisplayMedia({ video: constraints});

    document.getElementById("video").srcObject = stream;

    //Create peerconnection
    const pc = new RTCPeerConnection();

    //Send all tracks
    for (const track of stream.getTracks()) {
        //You could add simulcast too here
        pc.addTransceiver(track, {
            direction: "sendonly",
            streams:  [stream],
            // sendEncodings: [
            //     { rid: "0", active: true, maxBitrate: 8000000 },
                // { rid: "1", active: true, maxBitrate: 2000000 },
                // { rid: "2", active: true },
            // ],
        });
    }

    //Create whip client
    const whip = new WHIPClient();

    const url = "/whip/endpoint";
    const token = document.getElementById("room-id").value;

    //Start publishing
    whip.publish(pc, url, token);

    window.whip_instance = whip;
    window.stream_instance = stream;
}

window.stop = async () => {
    if (window.whip_instance) {
        window.whip_instance.stop();
    }

    if (window.stream_instance) {
        window.stream_instance.getTracks().forEach(track => track.stop());
    }

    document.getElementById("video").srcObject = null;
}