import java.security.KeyStore
import java.security.MessageDigest

plugins {
    id("com.android.application")
    id("kotlin-android")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

val releaseKeystore = System.getenv("PB_MAPPER_ANDROID_KEYSTORE")?.takeIf { it.isNotBlank() }

android {
    namespace = "com.pb_mapper.ui"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    kotlinOptions {
        jvmTarget = JavaVersion.VERSION_11.toString()
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("../../native/android")
        }
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "com.pb_mapper.ui"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
        
        // NOTE: When using --split-per-abi, do not set abiFilters here
        // as it conflicts with the split configuration.
        // If not using --split-per-abi, uncomment the following:
        // ndk {
        //     abiFilters += listOf("arm64-v8a", "armeabi-v7a")
        // }
    }

    // The maintainer's persistent key uses the Android debug keystore format
    // and credentials. CI supplies that same key as a temporary file.
    if (releaseKeystore != null) {
        signingConfigs.getByName("debug").storeFile = file(releaseKeystore)
    }

    buildTypes {
        release {
            // Local and CI releases must both use the pinned persistent key.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
}

gradle.taskGraph.whenReady {
    if (allTasks.any { it.name.contains("Release", ignoreCase = true) }) {
        check((System.getenv("CI") != "true" && System.getenv("GITHUB_ACTIONS") != "true") || releaseKeystore != null) {
            "CI release APKs require the persistent signing key; see docs/ui-updates.md"
        }
        check(android.signingConfigs.getByName("debug").storeFile?.isFile == true) {
            "The persistent Android signing key is missing. Restore its backup; do not generate a replacement."
        }
        val signing = android.signingConfigs.getByName("debug")
        val keyStore = KeyStore.getInstance("JKS")
        signing.storeFile!!.inputStream().use { keyStore.load(it, signing.storePassword!!.toCharArray()) }
        val certificate = keyStore.getCertificate(signing.keyAlias)
        val fingerprint = MessageDigest.getInstance("SHA-256")
            .digest(certificate.encoded).joinToString("") { "%02x".format(it) }
        val expected = rootProject.file("../../scripts/release/android_signing_certificate.sha256").readText().trim()
        check(fingerprint == expected) {
            "Android signing certificate changed. Restore the pinned signing key; see docs/ui-updates.md"
        }
    }
}

flutter {
    source = "../.."
}
