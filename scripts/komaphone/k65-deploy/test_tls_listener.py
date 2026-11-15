#!/usr/bin/env python3
"""Linux regression: distinguish a live listener from a closed TCP TIME_WAIT."""
import importlib.util
import pathlib
import socket
import unittest

spec = importlib.util.spec_from_file_location("renewal", pathlib.Path(__file__).with_name("tls-staging-renewal.py"))
renewal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(renewal)


class ListenerTests(unittest.TestCase):
    def test_live_listener_rejected(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            with self.assertRaises(AssertionError):
                renewal.no_stage_listener(listener.getsockname()[1])

    def test_closed_time_wait_is_not_listener(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            address = listener.getsockname()
            listener.listen()
            with socket.create_connection(address) as client:
                accepted, _ = listener.accept()
                with accepted:
                    accepted.shutdown(socket.SHUT_WR)
                    self.assertEqual(client.recv(1), b"")
        # The server actively closed; TCP retains its local port in TIME_WAIT.
        with socket.socket() as old_check:
            with self.assertRaises(OSError):
                old_check.bind(address)
        renewal.no_stage_listener(address[1])


if __name__ == "__main__":
    unittest.main()
