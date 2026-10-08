// SPDX-License-Identifier: GPL-2.0-only
use std::convert::TryFrom;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use mqttrs2::*;

/// A lightweight, robust MQTT 3.1.1 client using `mqttrs2` protocol codec
/// over a standard synchronous `TcpStream`.
pub struct MqttClient {
    stream: TcpStream,
    read_buf: Vec<u8>,
}

impl MqttClient {
    /// Connects to the local/remote MQTT broker and completes the MQTT 3.1.1 handshake.
    pub fn connect(addr: &str, client_id: &str) -> io::Result<Self> {
        let mut stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        // 1. Build and serialize CONNECT packet
        let connect = Connect {
            protocol: Protocol::MQTT311,
            keep_alive: 60,
            client_id,
            clean_session: true,
            last_will: None,
            username: None,
            password: None,
        };

        let mut send_buf = vec![0u8; 128 + client_id.len()];
        let n = encode_slice(&Packet::Connect(connect), &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT connect encode error: {e:?}")))?;
        stream.write_all(&send_buf[..n])?;

        // 2. Read and validate CONNACK packet
        let mut ack_buf = [0u8; 4];
        stream.read_exact(&mut ack_buf)?;
        let (_, pkt) = decode_slice_with_len(&ack_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT connack decode error: {e:?}")))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Incomplete CONNACK packet"))?;

        match pkt {
            Packet::Connack(connack) => {
                if connack.code != ConnectReturnCode::Accepted {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!("MQTT connection rejected, return code: {:?}", connack.code),
                    ));
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Expected CONNACK packet from broker",
                ));
            }
        }

        // Configure short timeout for polling in main worker loop
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;

        Ok(MqttClient {
            stream,
            read_buf: Vec::with_capacity(4096),
        })
    }

    /// Clones the underlying socket handle (useful for concurrent publishing while listening)
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(MqttClient {
            stream: self.stream.try_clone()?,
            read_buf: Vec::new(),
        })
    }

    /// Subscribes to a given MQTT topic path with QoS 0
    pub fn subscribe(&mut self, topic: &str) -> io::Result<()> {
        let sub = Subscribe {
            pid: Pid::try_from(1).unwrap_or_default(),
            topics: vec![SubscribeTopic {
                topic_path: topic.to_string(),
                qos: QoS::AtMostOnce,
            }],
        };

        let mut send_buf = vec![0u8; 128 + topic.len()];
        let n = encode_slice(&Packet::Subscribe(sub), &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT subscribe encode error: {e:?}")))?;
        self.stream.write_all(&send_buf[..n])
    }

    /// Publishes a payload to the designated topic with QoS 0
    pub fn publish(&mut self, topic: &str, data: &[u8]) -> io::Result<()> {
        let publish = Publish {
            dup: false,
            qospid: QosPid::AtMostOnce,
            retain: false,
            topic_name: topic,
            payload: data,
        };

        let mut send_buf = vec![0u8; 128 + topic.len() + data.len()];
        let n = encode_slice(&Packet::Publish(publish), &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT publish encode error: {e:?}")))?;
        self.stream.write_all(&send_buf[..n])
    }

    /// Sends a PINGREQ heartbeat packet (2 bytes)
    pub fn ping(&mut self) -> io::Result<()> {
        let mut send_buf = [0u8; 4];
        let n = encode_slice(&Packet::Pingreq, &mut send_buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("MQTT pingreq encode error: {e:?}")))?;
        self.stream.write_all(&send_buf[..n])
    }

    /// Streams and decodes incoming packets with full TCP framing / fragmentation handling.
    /// Returns `Ok(Some((topic, payload)))` when a PUBLISH packet arrives.
    /// Returns `Ok(None)` on read timeout (no data available right now).
    pub fn read_packet(&mut self) -> io::Result<Option<(String, Vec<u8>)>> {
        loop {
            // 1. Process any complete packets already buffered in `read_buf`
            if !self.read_buf.is_empty() {
                match decode_slice_with_len(&self.read_buf) {
                    Ok(Some((consumed_len, packet))) => {
                        let extracted_msg = match packet {
                            Packet::Publish(p) => Some((p.topic_name.to_string(), p.payload.to_vec())),
                            _ => None,
                        };
                        // Now packet borrow ends, we can drain the consumed bytes
                        self.read_buf.drain(..consumed_len);

                        if let Some(msg) = extracted_msg {
                            return Ok(Some(msg));
                        }
                        // Non-publish packet (Suback, Pingresp, etc.) was consumed, continue loop
                        continue;
                    }
                    Ok(None) => {
                        // Incomplete packet in buffer; proceed to read more bytes from socket
                    }
                    Err(e) => {
                        // Protocol framing error; clear buffer to prevent deadlocks
                        self.read_buf.clear();
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("MQTT stream decode error: {e:?}"),
                        ));
                    }
                }
            }

            // 2. Read incoming chunk from the TCP stream
            let mut chunk = [0u8; 2048];
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "MQTT broker closed connection",
                    ));
                }
                Ok(n) => {
                    self.read_buf.extend_from_slice(&chunk[..n]);
                    // Loop back to decode the newly accumulated bytes
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                    return Ok(None);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }
    }
}
