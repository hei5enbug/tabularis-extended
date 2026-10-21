self.onmessage = event => self.postMessage({ fixture: true, value: event.data });
