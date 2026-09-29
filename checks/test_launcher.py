import asyncio
import unittest
from unittest.mock import Mock, patch

import app


class LauncherTest(unittest.TestCase):
    def test_browser_opens_only_for_this_process(self):
        other = Mock(status=200)
        other.getheader.return_value = "another-instance"
        other.read.return_value = b"searchcord-ready"
        current = Mock(status=200)
        current.getheader.return_value = app.APP_INSTANCE_ID
        current.read.return_value = b"searchcord-ready"
        connections = [Mock(), Mock()]
        connections[0].getresponse.return_value = other
        connections[1].getresponse.return_value = current

        with patch.object(app, "HTTPConnection", side_effect=connections) as connect, \
                patch.object(app.webbrowser, "open") as open_browser, \
                patch.object(app.time, "sleep") as sleep:
            app.open_browser_when_ready("127.0.0.1", 8001)

        self.assertEqual(connect.call_count, 2)
        self.assertEqual(sleep.call_count, 1)
        other.read.assert_not_called()
        open_browser.assert_called_once_with("http://127.0.0.1:8001")
        for connection in connections:
            connection.close.assert_called_once()

    def test_health_marks_the_process(self):
        response = asyncio.run(app.health())
        self.assertEqual(response.body, b"searchcord-ready")
        self.assertEqual(response.headers["x-searchcord-instance"], app.APP_INSTANCE_ID)


if __name__ == "__main__":
    unittest.main()
