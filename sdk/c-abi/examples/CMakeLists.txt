cmake_minimum_required(VERSION 3.20)
project(fluvora_c_demo C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)

set(FLUVORA_LIBRARY_DIR "" CACHE PATH "Directory containing the built Fluvora C ABI library")
if(NOT FLUVORA_LIBRARY_DIR)
  message(FATAL_ERROR "Set FLUVORA_LIBRARY_DIR to target/release or target/debug")
endif()

find_library(
  FLUVORA_LIBRARY
  NAMES fluvora_c_abi fluvora-c-abi
  PATHS "${FLUVORA_LIBRARY_DIR}"
  NO_DEFAULT_PATH
  REQUIRED
)

add_executable(fluvora-c-demo room_client.c)
target_include_directories(fluvora-c-demo PRIVATE "${CMAKE_CURRENT_LIST_DIR}/../include")
target_compile_definitions(fluvora-c-demo PRIVATE FLUVORA_STATIC)
target_link_libraries(fluvora-c-demo PRIVATE "${FLUVORA_LIBRARY}")

if(UNIX AND NOT APPLE)
  target_link_libraries(fluvora-c-demo PRIVATE dl pthread m)
endif()
