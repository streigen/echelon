# GameActivity is referenced only from AndroidManifest.xml — R8 can't see it
-keep class com.google.androidgamesdk.GameActivity { *; }
