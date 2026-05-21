from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from host import host_bootloader as hb


class FakeSerial:
    def __init__(self) -> None:
        self._rx = bytearray()
        self._device = None

    def attach_device(self, device) -> None:
        self._device = device

    def inject_rx(self, data: bytes) -> None:
        if data:
            self._rx.extend(data)

    def read(self, size: int = 1) -> bytes:
        if size <= 0:
            return b""
        if not self._rx:
            return b""
        chunk = bytes(self._rx[:size])
        del self._rx[:size]
        return chunk

    def write(self, data: bytes) -> int:
        if self._device is None:
            raise RuntimeError("no device attached")
        self._device.on_host_bytes(data)
        return len(data)

    def flush(self) -> None:
        return None

    def reset_input_buffer(self) -> None:
        self._rx.clear()

    def close(self) -> None:
        return None


class BootloaderSim:
    STATUS_OK = 0
    STATUS_BAD_PACKET = 1
    STATUS_BAD_STATE = 2
    STATUS_BAD_OFFSET = 3
    STATUS_BAD_LENGTH = 4
    STATUS_BAD_CRC = 5

    def __init__(self, serial: FakeSerial, node_id: int = 1) -> None:
        self._ser = serial
        self.node_id = node_id
        self._rx = bytearray()

        self._state = "idle"
        self._image_size = 0
        self._image_crc32 = 0
        self._image_version = 0
        self._received = bytearray()
        self._expected_flash_offset = 0
        self._expected_valid_bytes = 0
        self._target_slot = 2  # pretend we're programming slot B

    @property
    def received_image(self) -> bytes:
        return bytes(self._received)

    def on_host_bytes(self, data: bytes) -> None:
        if data:
            self._rx.extend(data)

        while True:
            packet = hb.UpdateCodec.try_decode(self._rx)
            if packet is None:
                return
            self._handle_packet(packet)

    def _send_reply(
        self,
        packet_type: int,
        sequence: int,
        status: int,
        value: int,
        reserved0: int = 0,
    ) -> None:
        payload = hb.struct.pack(
            "<BBHI",
            status & 0xFF,
            reserved0 & 0xFF,
            0,
            value & 0xFFFFFFFF,
        )
        reply = hb.UpdateCodec.encode(packet_type, self.node_id, sequence, payload)
        self._ser.inject_rx(reply)

    def _handle_packet(self, packet: hb.UpdatePacket) -> None:
        if packet.destination != self.node_id:
            return

        if packet.packet_type == hb.UPDATE_TYPE_HELLO:
            packed = (
                (self.node_id << 24)
                | (hb.UPDATE_PROTOCOL_VERSION << 16)
                | 1024
            )
            self._state = "waiting_begin"
            self._send_reply(
                hb.UPDATE_TYPE_ACK,
                packet.sequence,
                0,
                packed,
                reserved0=self._target_slot,
            )
            return

        if packet.packet_type == hb.UPDATE_TYPE_BEGIN:
            if len(packet.payload) != 12:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, len(packet.payload))
                return
            self._image_size, self._image_crc32, self._image_version = hb.struct.unpack(
                "<III", packet.payload
            )
            self._received.clear()
            self._expected_flash_offset = 0
            self._expected_valid_bytes = 0
            self._state = "receiving"
            self._send_reply(hb.UPDATE_TYPE_ACK, packet.sequence, 0, self._target_slot)
            return

        if packet.packet_type == hb.UPDATE_TYPE_CHUNK:
            if self._state != "receiving":
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_STATE, 0)
                return

            if len(packet.payload) < 8:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, len(packet.payload))
                return

            offset, flash_length, valid_length = hb.struct.unpack("<IHH", packet.payload[:8])
            data = packet.payload[8:]

            if flash_length == 0 or valid_length == 0 or valid_length > flash_length:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, flash_length)
                return

            if flash_length % hb.FLASH_PAGE_SIZE != 0:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, flash_length)
                return

            if offset != self._expected_flash_offset:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_OFFSET, offset)
                return

            if len(data) != flash_length:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, len(data))
                return

            if data[valid_length:] != (b"\xFF" * (flash_length - valid_length)):
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_PACKET, valid_length)
                return

            self._received.extend(data[:valid_length])
            self._expected_flash_offset += flash_length
            self._expected_valid_bytes += valid_length
            self._send_reply(hb.UPDATE_TYPE_ACK, packet.sequence, 0, offset + valid_length)
            return

        if packet.packet_type == hb.UPDATE_TYPE_END:
            if self._state != "receiving":
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_STATE, 0)
                return
            if len(packet.payload) != 4:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, len(packet.payload))
                return

            (expected_crc32,) = hb.struct.unpack("<I", packet.payload)
            if expected_crc32 != self._image_crc32:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_CRC, expected_crc32)
                return

            if len(self._received) != self._image_size:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_LENGTH, len(self._received))
                return

            computed = hb.update_crc32(bytes(self._received))
            if computed != self._image_crc32:
                self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, self.STATUS_BAD_CRC, computed)
                return

            self._state = "completed"
            self._send_reply(hb.UPDATE_TYPE_ACK, packet.sequence, 0, self._target_slot)
            return

        if packet.packet_type == hb.UPDATE_TYPE_ABORT:
            self._state = "aborted"
            self._send_reply(hb.UPDATE_TYPE_ACK, packet.sequence, 0, 0)
            return

        self._send_reply(hb.UPDATE_TYPE_ERROR, packet.sequence, 1, packet.packet_type)


class HostBootloaderTests(unittest.TestCase):
    def test_transport_codec_resync(self) -> None:
        payload = b"\x01\x02\x03"
        encoded = hb.TransportCodec.encode(
            frame_type=hb.FRAME_TYPE_RESPONSE,
            destination=hb.HOST_NODE_ID,
            source=1,
            sequence=123,
            payload=payload,
        )
        rx = bytearray(b"\x00\x11\x22\x33" + encoded)
        frame = hb.TransportCodec.try_decode(rx)
        self.assertIsNotNone(frame)
        assert frame is not None
        self.assertEqual(frame.payload, payload)
        self.assertEqual(frame.sequence, 123)
        self.assertEqual(frame.source, 1)

    def test_update_codec_resync(self) -> None:
        payload = b"abc"
        encoded = hb.UpdateCodec.encode(hb.UPDATE_TYPE_ACK, 1, 7, payload)
        rx = bytearray(b"\x99\x88" + encoded)
        packet = hb.UpdateCodec.try_decode(rx)
        self.assertIsNotNone(packet)
        assert packet is not None
        self.assertEqual(packet.packet_type, hb.UPDATE_TYPE_ACK)
        self.assertEqual(packet.destination, 1)
        self.assertEqual(packet.sequence, 7)
        self.assertEqual(packet.payload, payload)

    def test_update_protocol_upload_end_to_end(self) -> None:
        fake = FakeSerial()
        dev = BootloaderSim(fake, node_id=1)
        fake.attach_device(dev)

        client = hb.UpdateClient(fake)
        node, proto, max_chunk, target_slot = hb.send_hello(client, 1, timeout_s=0.2)
        self.assertEqual(node, 1)
        self.assertEqual(proto, hb.UPDATE_PROTOCOL_VERSION)
        self.assertEqual(max_chunk, 1024)
        self.assertEqual(target_slot, hb.SLOT_B)

        image = bytes(range(251)) + b"\x00\x01\x02" + b"tail"
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "fw.bin"
            path.write_bytes(image)
            hb.upload_image(client, 1, path, version=0x1234, timeout_s=0.2, chunk_size=500)

        self.assertEqual(dev.received_image, image)

    def test_abort(self) -> None:
        fake = FakeSerial()
        dev = BootloaderSim(fake, node_id=1)
        fake.attach_device(dev)

        client = hb.UpdateClient(fake)
        hb.send_hello(client, 1, timeout_s=0.2)
        hb.abort_update(client, 1, timeout_s=0.2)


if __name__ == "__main__":
    unittest.main()
