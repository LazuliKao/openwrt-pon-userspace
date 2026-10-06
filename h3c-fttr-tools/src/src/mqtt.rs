// SPDX-License-Identifier: GPL-2.0-only
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct MqttClient {
    stream: TcpStream,
}

impl MqttClient {
    pub fn connect(addr: &str, client_id: &str) -> io::Result<Self> {
        let mut stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        // Construct CONNECT packet
        let mut payload = Vec::new();
        // Client ID string
        payload.extend_from_slice(&(client_id.len() as u16).to_be_bytes());
        payload.extend_from_slice(client_id.as_bytes());

        let mut var_header = Vec::new();
        var_header.extend_from_slice(&[0x00, 0x04, b'M', b'Q', b'T', b'T']); // Protocol Name
        var_header.push(0x04); // Protocol Level (MQTT 3.1.1)
        var_header.push(0x02); // Clean Session flag
        var_header.extend_from_slice(&60u16.to_be_bytes()); // Keep Alive (60s)

        let mut packet = vec![0x10]; // CONNECT packet type
        let rem_len = var_header.len() + payload.len();
        packet.extend(encode_remaining_length(rem_len));
        packet.extend(var_header);
        packet.extend(payload);

        stream.write_all(&packet)?;

        // Read CONNACK (0x20, 0x02, session_present, return_code)
        let mut ack = [0u8; 4];
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.read_exact(&mut ack)?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;

        if ack[0] != 0x20 || ack[3] != 0x00 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("MQTT connection rejected, return code: {}", ack[3]),
            ));
        }

        Ok(MqttClient { stream })
    }

    pub fn subscribe(&mut self, topic: &str) -> io::Result<()> {
        let mut var_header = Vec::new();
        var_header.extend_from_slice(&1u16.to_be_bytes()); // Packet ID = 1

        let mut payload = Vec::new();
        payload.extend_from_slice(&(topic.len() as u16).to_be_bytes());
        payload.extend_from_slice(topic.as_bytes());
        payload.push(0x00); // Requested QoS 0

        let mut packet = vec![0x82]; // SUBSCRIBE packet type
        let rem_len = var_header.len() + payload.len();
        packet.extend(encode_remaining_length(rem_len));
        packet.extend(var_header);
        packet.extend(payload);

        self.stream.write_all(&packet)
    }

    pub fn publish(&mut self, topic: &str, data: &[u8]) -> io::Result<()> {
        let mut var_header = Vec::new();
        var_header.extend_from_slice(&(topic.len() as u16).to_be_bytes());
        var_header.extend_from_slice(topic.as_bytes());

        let mut packet = vec![0x30]; // PUBLISH packet type (QoS 0)
        let rem_len = var_header.len() + data.len();
        packet.extend(encode_remaining_length(rem_len));
        packet.extend(var_header);
        packet.extend_from_slice(data);

        self.stream.write_all(&packet)
    }

    pub fn ping(&mut self) -> io::Result<()> {
        self.stream.write_all(&[0xC0, 0x00])
    }

    pub fn read_packet(&mut self) -> io::Result<Option<(String, Vec<u8>)>> {
        let mut header = [0u8; 1];
        match self.stream.read_exact(&mut header) {
            Ok(()) => {}
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        }

        let packet_type = header[0] >> 4;
        let rem_len = decode_remaining_length(&mut self.stream)?;

        if packet_type == 3 {
            // PUBLISH packet
            let mut buf = vec![0u8; rem_len];
            self.stream.read_exact(&mut buf)?;

            if buf.len() < 2 {
                return Ok(None);
            }
            let topic_len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
            if buf.len() < 2 + topic_len {
                return Ok(None);
            }
            let topic = String::from_utf8_lossy(&buf[2..2 + topic_len]).to_string();
            let payload = buf[2 + topic_len..].to_vec();

            return Ok(Some((topic, payload)));
        } else {
            // Drain other packets (e.g. SUBACK, PINGRESP)
            let mut drain = vec![0u8; rem_len];
            let _ = self.stream.read_exact(&mut drain);
        }

        Ok(None)
    }
}

fn encode_remaining_length(mut len: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 128;
        }
        bytes.push(byte);
        if len == 0 {
            break;
        }
    }
    bytes
}

fn decode_remaining_length(stream: &mut TcpStream) -> io::Result<usize> {
    let mut multiplier = 1;
    let mut value = 0;
    let mut byte_buf = [0u8; 1];

    loop {
        stream.read_exact(&mut byte_buf)?;
        let byte = byte_buf[0];
        value += ((byte & 127) as usize) * multiplier;
        if (byte & 128) == 0 {
            break;
        }
        multiplier *= 128;
    }
    Ok(value)
}
