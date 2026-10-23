package flash.net {
    import flash.events.EventDispatcher;
    import flash.net.URLRequest;

    public class URLLoader extends EventDispatcher {
        [Ruffle(NativeAccessible)]
        public var data:*;

        [Ruffle(NativeAccessible)]
        public var dataFormat:String = "text";

        [Ruffle(NativeAccessible)]
        public var bytesLoaded:uint;

        [Ruffle(NativeAccessible)]
        public var bytesTotal:uint;

        // Incremented by every `load()` and `close()`. A response is only delivered
        // if this still matches the value its load started with, so a new load or a
        // close cancels the previous one, like in Flash Player.
        [Ruffle(NativeAccessible)]
        private var _loadId:uint = 0;

        public function URLLoader(request:URLRequest = null) {
            if (request != null) {
                this.load(request);
            }
        }

        public native function load(request:URLRequest):void;

        public function close():void {
            this._loadId++;
        }
    }
}
